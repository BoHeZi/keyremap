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

use crate::keycode::{is_extended_key, normalize_for_inject};

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
