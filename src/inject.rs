//! 按键注入。基于 `SendInput`。
//!
//! 两个关键设计:
//!
//! 1. 每个注入事件的 `dwExtraInfo` 都打上 [`INJECTED_TAG`], 钩子回调据此识别
//!    "这是我自己发出去的", 直接放行。没有这一步, A->B 和 B->A 同时启用会无限回环。
//!    AutoHotkey 用的是同一手法 (它的常量叫 `KEY_IGNORE`)。
//!
//! 2. 组合键用**一次** `SendInput` 发送完整序列。该调用对整批事件是原子的,
//!    系统保证中途不会插入其他输入 —— 因此不需要在事件之间 sleep。
//!    低级钩子回调有 `LowLevelHooksTimeout` (默认 300ms) 限制, 超时会被系统
//!    静默摘掉钩子, 所以回调里绝不能睡。

use std::mem::size_of;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput,
};

use crate::keycode::{MAX_MODS, held_mod_vks, is_extended_key, normalize_for_inject};

/// 自注入事件的标记, 写进 `dwExtraInfo`。取值任意, 只要够独特。
/// 这里是 "KRMP" 四个字母的 ASCII。
pub const INJECTED_TAG: usize = 0x4B52_4D50;

/// 组合键最多支持的键数。定成常量是为了在栈上开固定数组,
/// 避免在钩子回调里做堆分配。
pub const MAX_COMBO: usize = 8;

/// 构造一个键盘 INPUT。
fn key_input(vk: u16, up: bool) -> INPUT {
    let mut flags = 0u32;
    if is_extended_key(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: normalize_for_inject(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            },
        },
    }
}

/// 发送一批已构造好的 INPUT。返回是否全部成功。
fn send(inputs: &[INPUT]) -> bool {
    if inputs.is_empty() {
        return true;
    }
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    sent as usize == inputs.len()
}

/// 注入单个按键的按下或抬起。
pub fn send_key(vk: u16, up: bool) -> bool {
    send(&[key_input(vk, up)])
}

/// 注入一个组合键: 顺序按下, 反序抬起, 一次调用原子完成。
///
/// 例如 `[Ctrl, W]` 会发出 `Ctrl↓ W↓ W↑ Ctrl↑`。
pub fn send_combination(vks: &[u16]) -> bool {
    let n = vks.len().min(MAX_COMBO);
    if n == 0 {
        return true;
    }

    // 栈上固定数组, 不做堆分配 —— 这是钩子回调里的热路径。
    let mut buf: [INPUT; MAX_COMBO * 2] = unsafe { std::mem::zeroed() };
    let mut len = 0;

    for &vk in vks.iter().take(n) {
        buf[len] = key_input(vk, false);
        len += 1;
    }
    for &vk in vks.iter().take(n).rev() {
        buf[len] = key_input(vk, true);
        len += 1;
    }

    send(&buf[..len])
}

/// 触发一条**以组合键为输入源**的映射: 先松开被占用的修饰键, 发出目标键, 再按回去。
///
/// 为什么必须先松开: 用户按下 `Ctrl` 的那一下**已经放行给应用了**。此时直接注入
/// `Backspace`, 应用收到的是 `Ctrl+Backspace` —— 多数编辑器会删掉一整个词,
/// 而不是用户想要的一个字符。
///
/// `mods` 只包含该映射在 `from` 里点名的修饰键。用户同时按着的其他修饰键
/// (比如 Shift) 是他自己要的, 不动。
///
/// 松开与按回的都是[**当前实际按着**的那几个键][held_mod_vks] —— 用户按的是
/// 右 Ctrl 就松右 Ctrl。按固定的左键处理会既松不掉 (右边还按着) 又留下一个
/// 卡住的左 Ctrl, 详见那个函数的说明。
///
/// 整批放进**一次** `SendInput`: 该调用对整批事件是原子的, 中途不会有别的输入
/// 插进来, 应用也就看不到"Ctrl 松了但还没按回去"这种中间状态。
pub fn send_with_mods_released(mods: u16, vks: &[u16]) -> bool {
    let n = vks.len().min(MAX_COMBO);
    if n == 0 {
        return true;
    }
    let (mod_keys, mod_n) = held_mod_vks(mods);

    // 目标序列里本来就要按的修饰键, 不必松开再按回 —— 那样应用会平白看到一次
    // 抖动。`Ctrl+Q -> Ctrl+W` 是最典型的例子: 用户按着的 Ctrl 正是目标要的,
    // 直接留着就好, 只发 W 即可。
    //
    // 只在虚拟键**完全相同**时才这么省。用户按的是右 Ctrl、目标写的是通用
    // `Ctrl` (注入时落到左 Ctrl) 就不算 —— 那种情况老老实实松开再按回更稳妥,
    // 尤其 RAlt 在很多键盘布局上是 AltGr, 与左 Alt 并不等价。
    let keep = |vk: u16| vks[..n].iter().any(|&t| normalize_for_inject(t) == vk);

    // 栈上固定数组: 松开修饰键 + 目标序列按下抬起 + 按回修饰键
    let mut buf: [INPUT; MAX_MODS * 2 + MAX_COMBO * 2] = unsafe { std::mem::zeroed() };
    let mut len = 0;

    for &vk in mod_keys.iter().take(mod_n) {
        if !keep(vk) {
            buf[len] = key_input(vk, true);
            len += 1;
        }
    }
    // 已经按着的那几个修饰键在目标序列里也跳过, 否则会多出一次按下与抬起
    for &vk in vks.iter().take(n) {
        if !mod_keys[..mod_n].contains(&normalize_for_inject(vk)) {
            buf[len] = key_input(vk, false);
            len += 1;
        }
    }
    for &vk in vks.iter().take(n).rev() {
        if !mod_keys[..mod_n].contains(&normalize_for_inject(vk)) {
            buf[len] = key_input(vk, true);
            len += 1;
        }
    }
    // 反序按回, 与松开的顺序对称
    for &vk in mod_keys.iter().take(mod_n).rev() {
        if !keep(vk) {
            buf[len] = key_input(vk, false);
            len += 1;
        }
    }

    send(&buf[..len])
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_BACK, VK_LCONTROL, VK_LSHIFT, VK_RCONTROL,
    };

    /// 从构造好的 INPUT 批次里读出 (虚拟键, 是否抬起), 便于断言序列。
    fn seq(inputs: &[INPUT]) -> Vec<(u16, bool)> {
        inputs
            .iter()
            .map(|i| unsafe {
                (
                    i.Anonymous.ki.wVk,
                    i.Anonymous.ki.dwFlags & KEYEVENTF_KEYUP != 0,
                )
            })
            .collect()
    }

    /// 复刻 send_with_mods_released 的排列逻辑, 但把"当前按着哪些修饰键"
    /// 作为参数传进来 —— 真实函数那一步要查系统状态, 测试里没法造。
    fn build(held: &[u16], vks: &[u16]) -> Vec<(u16, bool)> {
        let keep = |vk: u16| vks.iter().any(|&t| normalize_for_inject(t) == vk);
        let already = |vk: u16| held.contains(&normalize_for_inject(vk));

        let mut buf = Vec::new();
        for &vk in held.iter().filter(|&&vk| !keep(vk)) {
            buf.push(key_input(vk, true));
        }
        for &vk in vks.iter().filter(|&&vk| !already(vk)) {
            buf.push(key_input(vk, false));
        }
        for &vk in vks.iter().rev().filter(|&&vk| !already(vk)) {
            buf.push(key_input(vk, true));
        }
        for &vk in held.iter().rev().filter(|&&vk| !keep(vk)) {
            buf.push(key_input(vk, false));
        }
        seq(&buf)
    }

    #[test]
    fn 修饰键先松后按且目标键夹在中间() {
        // Ctrl+E -> Backspace: 应用必须收到干净的 Backspace, 而不是 Ctrl+Backspace
        assert_eq!(
            build(&[VK_LCONTROL], &[VK_BACK]),
            vec![
                (VK_LCONTROL, true),  // 松开 Ctrl
                (VK_BACK, false),     // Backspace 按下
                (VK_BACK, true),      // Backspace 抬起
                (VK_LCONTROL, false), // Ctrl 按回去
            ]
        );
    }

    #[test]
    fn 松开与按回的是同一批键() {
        // 用户按的是右 Ctrl, 那就得松右 Ctrl、按回右 Ctrl。
        // 若这里换成固定的左键, 既松不掉 (右边还按着) 又会留下一个卡住的左 Ctrl。
        let s = build(&[VK_RCONTROL], &[VK_BACK]);
        assert_eq!(s.first(), Some(&(VK_RCONTROL, true)));
        assert_eq!(s.last(), Some(&(VK_RCONTROL, false)));
        assert!(
            !s.iter().any(|(vk, _)| *vk == VK_LCONTROL),
            "不该碰用户没按的那一侧: {s:?}"
        );
    }

    #[test]
    fn 目标也要的修饰键就留着不动() {
        // Ctrl+Q -> Ctrl+W: 用户按着的 Ctrl 正是目标要的, 留着即可,
        // 松开再按回只会让应用平白看到一次抖动
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_W};
        let s = build(&[VK_LCONTROL], &[VK_CONTROL, VK_W]);
        assert_eq!(s, vec![(VK_W, false), (VK_W, true)], "只该发出 W: {s:?}");
    }

    #[test]
    fn 只留下目标要的那个其余照旧松开() {
        // Ctrl+Shift+X -> Ctrl+C: Ctrl 留着, Shift 必须松开,
        // 否则应用收到的是 Ctrl+Shift+C
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_C, VK_CONTROL};
        let s = build(&[VK_LCONTROL, VK_LSHIFT], &[VK_CONTROL, VK_C]);
        assert_eq!(
            s,
            vec![
                (VK_LSHIFT, true),
                (VK_C, false),
                (VK_C, true),
                (VK_LSHIFT, false),
            ]
        );
    }

    #[test]
    fn 左右不同时不做这个省略() {
        // 按的是右 Ctrl 而目标写的是通用 Ctrl (注入落到左 Ctrl), 两者并不是
        // 同一个键。老实松开再按回, 结果依然正确
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_W};
        let s = build(&[VK_RCONTROL], &[VK_CONTROL, VK_W]);
        assert_eq!(s.first(), Some(&(VK_RCONTROL, true)));
        assert_eq!(s.last(), Some(&(VK_RCONTROL, false)));
        assert!(s.iter().any(|(vk, _)| *vk == VK_LCONTROL), "{s:?}");
    }

    #[test]
    fn 多个修饰键按对称顺序松开与按回() {
        let s = build(&[VK_LCONTROL, VK_LSHIFT], &[VK_BACK]);
        assert_eq!(s.first(), Some(&(VK_LCONTROL, true)));
        assert_eq!(s.get(1), Some(&(VK_LSHIFT, true)));
        // 按回来时反序, 和松开对称
        assert_eq!(s[s.len() - 2], (VK_LSHIFT, false));
        assert_eq!(s[s.len() - 1], (VK_LCONTROL, false));
    }

    #[test]
    fn 没有修饰键按着时就是一次普通组合键() {
        assert_eq!(
            build(&[], &[VK_BACK]),
            vec![(VK_BACK, false), (VK_BACK, true)]
        );
    }

    #[test]
    fn 缓冲区容得下最多修饰键与最长组合() {
        // 8 个修饰键松开 + 8 键按下 + 8 键抬起 + 8 个修饰键按回
        let held = [VK_LCONTROL; MAX_MODS];
        let vks = [VK_BACK; MAX_COMBO];
        assert_eq!(
            build(&held, &vks).len(),
            MAX_MODS * 2 + MAX_COMBO * 2,
            "缓冲区必须放得下最坏情况"
        );
    }
}
