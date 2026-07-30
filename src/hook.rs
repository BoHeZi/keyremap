//! 低级键盘/鼠标钩子。
//!
//! 架构要点:
//!
//! - 钩子回调是 `extern "system" fn`, 无法捕获环境, 所以配置放在全局的
//!   `RwLock<Arc<Config>>` 里。回调只做"取 Arc 副本"这一次短暂持锁, 随后立刻放锁,
//!   为后续的热重载留好了替换入口 ([`set_config`])。
//!
//! - 总开关是一个 `AtomicBool`。低级钩子**不需要卸载再重装**, 关闭时回调直接放行即可,
//!   代价是一次原子读。托盘菜单的"启用/禁用"就接在这里。
//!
//! - 回调里绝不 sleep、绝不做字符串操作、绝不 panic。系统全局的按键都在等它返回,
//!   超过 `LowLevelHooksTimeout` (默认 300ms) 钩子会被静默摘除。

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use log::{debug, info, warn};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    XBUTTON1,
};

use crate::config::Config;
use crate::inject::{self, INJECTED_TAG};
use crate::keycode::{Input, MouseButton, name_from_mouse, name_from_vk};

/// 当前生效的配置。用 RwLock 包 Arc: 回调侧只读并克隆 Arc, 写侧 (热重载) 整体替换。
static CONFIG: OnceLock<RwLock<Arc<Config>>> = OnceLock::new();

/// 总开关。托盘菜单的"启用/禁用"改这个值。
static ENABLED: AtomicBool = AtomicBool::new(true);

/// 监听模式: 只打印不拦截, 用于让用户查出某个键叫什么名字。
static LISTEN_ONLY: AtomicBool = AtomicBool::new(false);

/// 已安装的钩子句柄, 供退出时卸载。0 表示未安装。
static KEYBOARD_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);

// ---------- 对外状态接口 ----------

pub fn set_config(config: Config) {
    let cell = CONFIG.get_or_init(|| RwLock::new(Arc::new(Config::default())));
    match cell.write() {
        Ok(mut guard) => *guard = Arc::new(config),
        Err(e) => warn!("配置更新失败, 锁已中毒: {e}"),
    }
}

fn current_config() -> Option<Arc<Config>> {
    // 回调热路径: 只在这里短暂持读锁, 克隆 Arc 后立刻释放。
    CONFIG.get()?.read().ok().map(|g| g.clone())
}

/// 取当前配置的快照, 供托盘构建菜单。
pub fn config_snapshot() -> Option<Arc<Config>> {
    current_config()
}

/// 切换第 `index` 条映射的启用状态, 返回切换后的值。
///
/// 只改内存不写回文件: 写回会触发文件监听再触发重载, 形成回路;
/// 而且托盘上的开关更像"临时静音", 重载配置时回到文件里写的状态是合理的。
///
/// 用 copy-on-write 整体替换 Arc, 而不是原地改 —— 这样钩子回调侧永远
/// 看到的是一个完整一致的配置, 不需要在热路径上加写锁。
pub fn toggle_mapping(index: usize) -> Option<bool> {
    let cell = CONFIG.get()?;
    let mut guard = cell.write().ok()?;

    let mut next = (**guard).clone();
    let m = next.mappings.get_mut(index)?;
    m.enable = !m.enable;
    let now = m.enable;

    *guard = Arc::new(next);
    Some(now)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    info!("映射已{}", if on { "启用" } else { "禁用" });
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_listen_only(on: bool) {
    LISTEN_ONLY.store(on, Ordering::Relaxed);
}

// ---------- 匹配与处理 ----------

/// 处理一个输入事件, 返回 true 表示该事件应被拦截 (不再传给后续应用)。
fn handle(input: Input, is_down: bool) {
    if LISTEN_ONLY.load(Ordering::Relaxed) {
        let name = match input {
            Input::Key(vk) => name_from_vk(vk)
                .map(str::to_string)
                .unwrap_or_else(|| format!("未知键(VK=0x{vk:02X})")),
            Input::Mouse(btn) => name_from_mouse(btn).to_string(),
        };
        info!("{name}  [{}]", if is_down { "按下" } else { "抬起" });
    }
}

/// 查找并执行匹配的映射。返回 true 表示原事件应被吞掉。
fn dispatch(input: Input, is_down: bool) -> bool {
    handle(input, is_down);

    if LISTEN_ONLY.load(Ordering::Relaxed) || !ENABLED.load(Ordering::Relaxed) {
        return false;
    }

    let Some(config) = current_config() else {
        return false;
    };

    for m in &config.mappings {
        if !m.enable || m.from != input {
            continue;
        }

        if m.is_combo() {
            // 组合键在**抬起**时触发一次。
            // 若改在按下时触发, 长按会被系统的按键重复反复触发 ——
            // 对 Ctrl+W 这类破坏性操作意味着连关一串标签页。
            if !is_down {
                inject::send_combination(&m.to);
            }
        } else {
            // 单键映射按下/抬起分别透传, 从而保留长按自动重复的自然手感。
            inject::send_key(m.to[0], !is_down);
        }
        return true;
    }
    false
}

// ---------- 钩子回调 ----------

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };

        // 自己注入的事件直接放行 —— 没有这一句, A->B 与 B->A 会无限回环。
        if kb.dwExtraInfo != INJECTED_TAG {
            let msg = wparam as u32;
            let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

            if (is_down || is_up) && dispatch(Input::Key(kb.vkCode as u16), is_down) {
                return 1; // 非 0 即拦截, 事件不再向下传递
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let msg = wparam as u32;

        // 鼠标移动是最高频的事件, 在这里用一次整数比较甩掉,
        // 连 MSLLHOOKSTRUCT 都不去解引用。
        if msg != WM_MOUSEMOVE {
            let ms = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
            if ms.dwExtraInfo != INJECTED_TAG
                && let Some((btn, is_down)) = decode_mouse(msg, ms.mouseData)
                && dispatch(Input::Mouse(btn), is_down)
            {
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

/// 把鼠标消息解码成 (按键, 是否按下)。侧键要从 mouseData 的高 16 位取。
fn decode_mouse(msg: u32, mouse_data: u32) -> Option<(MouseButton, bool)> {
    let btn = match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP => MouseButton::Left,
        WM_RBUTTONDOWN | WM_RBUTTONUP => MouseButton::Right,
        WM_MBUTTONDOWN | WM_MBUTTONUP => MouseButton::Middle,
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            if (mouse_data >> 16) as u16 == XBUTTON1 {
                MouseButton::X1
            } else {
                MouseButton::X2
            }
        }
        _ => return None,
    };
    let is_down = matches!(
        msg,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
    );
    Some((btn, is_down))
}

// ---------- 安装 / 卸载 / 消息循环 ----------

/// 安装钩子。`with_mouse` 为 false 时不装鼠标钩子, 省掉全部鼠标事件的回调开销。
pub fn install(with_mouse: bool) -> Result<(), String> {
    unsafe {
        let kh = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), ptr::null_mut(), 0);
        if kh.is_null() {
            return Err("安装键盘钩子失败".into());
        }
        KEYBOARD_HOOK.store(kh as isize, Ordering::SeqCst);
        debug!("键盘钩子已安装");

        if with_mouse {
            let mh = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), ptr::null_mut(), 0);
            if mh.is_null() {
                return Err("安装鼠标钩子失败".into());
            }
            MOUSE_HOOK.store(mh as isize, Ordering::SeqCst);
            debug!("鼠标钩子已安装");
        } else {
            debug!("配置中无鼠标映射, 跳过鼠标钩子");
        }
    }
    Ok(())
}

pub fn uninstall() {
    for slot in [&KEYBOARD_HOOK, &MOUSE_HOOK] {
        let h = slot.swap(0, Ordering::SeqCst);
        if h != 0 {
            unsafe { UnhookWindowsHookEx(h as *mut _) };
        }
    }
    debug!("钩子已卸载");
}

/// 消息循环。低级钩子的回调由系统在本线程的消息处理中派发,
/// 没有这个循环钩子就不会被调用 —— 这也是后续接入托盘窗口消息的地方。
pub fn run_message_loop() {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe {
        // GetMessageW 返回 0 表示 WM_QUIT, -1 表示出错
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解码鼠标侧键() {
        // XBUTTON2 在 mouseData 的高 16 位
        let data = (2u32) << 16;
        assert_eq!(
            decode_mouse(WM_XBUTTONDOWN, data),
            Some((MouseButton::X2, true))
        );
        let data1 = (1u32) << 16;
        assert_eq!(
            decode_mouse(WM_XBUTTONUP, data1),
            Some((MouseButton::X1, false))
        );
    }

    #[test]
    fn 解码普通鼠标键() {
        assert_eq!(
            decode_mouse(WM_LBUTTONDOWN, 0),
            Some((MouseButton::Left, true))
        );
        assert_eq!(
            decode_mouse(WM_MBUTTONUP, 0),
            Some((MouseButton::Middle, false))
        );
    }

    #[test]
    fn 忽略无关鼠标消息() {
        assert_eq!(decode_mouse(WM_MOUSEMOVE, 0), None);
    }
}
