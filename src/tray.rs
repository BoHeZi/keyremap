//! 托盘图标与右键菜单, 直接基于 Shell_NotifyIconW 实现。
//!
//! 几个关键决定:
//!
//! - **菜单每次右键时才构建**。不预先建好再去同步勾选状态 —— 那样菜单显示的状态
//!   与实际生效的状态存在失步的可能, 表现为"点了开关却好像没反应"。
//!   动态构建时菜单永远是当前真实状态的投影。
//!
//! - 用 `TPM_RETURNCMD` 让 `TrackPopupMenu` 直接返回被点中的 ID, 不绕 `WM_COMMAND`。
//!
//! - 承载托盘的是**普通隐藏窗口**, 不是 `HWND_MESSAGE` 消息窗口: 后者不能成为
//!   前台窗口, `SetForegroundWindow` 会失败, 导致弹出菜单在点击别处时不消失。

use std::ffi::OsStr;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

use log::{debug, error, info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, IDI_APPLICATION, LoadIconW, MF_CHECKED, MF_SEPARATOR, MF_STRING, PostQuitMessage,
    RegisterClassW, SW_SHOWNORMAL, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
};

use crate::{hook, watcher};

/// 托盘图标回调消息。图标上的鼠标动作都通过它送到窗口过程。
const WM_TRAY_CALLBACK: u32 = WM_APP + 100;

/// 托盘图标在本窗口内的唯一编号。只有一个图标, 固定即可。
const TRAY_ICON_ID: u32 = 1;

// 菜单命令 ID。映射项从 ID_MAPPING_BASE 开始按下标顺延,
// 这样不用保存任何 id 表, 反解一次减法就够。
const ID_TOGGLE: u32 = 1;
const ID_RELOAD: u32 = 2;
const ID_OPEN_FILE: u32 = 3;
const ID_OPEN_DIR: u32 = 4;
const ID_QUIT: u32 = 5;
const ID_MAPPING_BASE: u32 = 100;

/// 隐藏窗口的句柄。窗口过程与主线程都在同一个线程上跑, 这里只是为了让
/// 静态变量满足 Sync —— HWND 本身不是 Send。
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

/// 配置文件路径, 供"重新加载""打开配置"等菜单项使用。
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// 创建隐藏窗口并注册托盘图标。
pub fn init(config_path: &Path) -> Result<(), String> {
    let _ = CONFIG_PATH.set(config_path.to_path_buf());

    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let class_name = wide("keyremap_ng_tray_window");

        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        // 重复注册会失败, 但本进程只会调用一次, 失败直接当错处理即可
        if RegisterClassW(&class) == 0 {
            return Err("注册窗口类失败".into());
        }

        // 不带 WS_VISIBLE, 窗口不会显示; 但它是普通窗口而非 HWND_MESSAGE 窗口,
        // 这样 SetForegroundWindow 才能成功, 弹出菜单的行为才正常。
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("keyremap-ng").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            hinst,
            ptr::null(),
        );
        if hwnd.is_null() {
            return Err("创建托盘窗口失败".into());
        }
        TRAY_HWND.store(hwnd as isize, Ordering::SeqCst);

        let mut data = icon_data(hwnd);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_TRAY_CALLBACK;
        data.hIcon = load_app_icon();
        fill_wide(&mut data.szTip, &status_text());

        if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
            DestroyWindow(hwnd);
            return Err("添加托盘图标失败".into());
        }
        debug!("托盘图标已创建");
    }
    Ok(())
}

/// 移除托盘图标并销毁窗口。
pub fn shutdown() {
    let hwnd = TRAY_HWND.swap(0, Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let data = icon_data(hwnd as HWND);
        Shell_NotifyIconW(NIM_DELETE, &data);
        DestroyWindow(hwnd as HWND);
    }
    debug!("托盘图标已移除");
}

/// 刷新悬停提示。状态变化后调用。
pub fn update_status() {
    let hwnd = TRAY_HWND.load(Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let mut data = icon_data(hwnd as HWND);
        data.uFlags = NIF_TIP;
        fill_wide(&mut data.szTip, &status_text());
        Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

/// 弹出气泡通知。配置重载、开关切换这类事件用它给出反馈。
pub fn notify(title: &str, message: &str) {
    let hwnd = TRAY_HWND.load(Ordering::SeqCst);
    if hwnd == 0 {
        return;
    }
    unsafe {
        let mut data = icon_data(hwnd as HWND);
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        fill_wide(&mut data.szInfoTitle, title);
        fill_wide(&mut data.szInfo, message);
        Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

fn status_text() -> String {
    let n = hook::config_snapshot()
        .map(|c| c.enabled_count())
        .unwrap_or(0);
    format!(
        "keyremap-ng — {} ({n} 条映射生效)",
        if hook::is_enabled() {
            "已启用"
        } else {
            "已禁用"
        }
    )
}

// ---------- 窗口过程 ----------

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY_CALLBACK => {
            // 左右键都弹菜单: 托盘图标没有"主操作", 左键弹菜单比什么都不做有用
            let action = lparam as u32;
            if action == WM_RBUTTONUP || action == WM_LBUTTONUP {
                unsafe { show_menu(hwnd) };
            }
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 构建并弹出菜单, 直接拿到被点中的命令。
unsafe fn show_menu(hwnd: HWND) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        warn!("创建菜单失败");
        return;
    }

    unsafe {
        let checked = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };

        let label = wide("启用映射");
        AppendMenuW(
            menu,
            checked(hook::is_enabled()),
            ID_TOGGLE as usize,
            label.as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());

        // 逐条映射的开关。标题里带上映射内容, 一眼能看出哪条是哪条。
        if let Some(cfg) = hook::config_snapshot() {
            for (i, m) in cfg.mappings.iter().enumerate() {
                let w = wide(&m.label());
                AppendMenuW(
                    menu,
                    checked(m.enable),
                    (ID_MAPPING_BASE + i as u32) as usize,
                    w.as_ptr(),
                );
            }
            if !cfg.mappings.is_empty() {
                AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
            }
        }

        for (id, text) in [
            (ID_RELOAD, "重新加载配置"),
            (ID_OPEN_FILE, "打开配置文件"),
            (ID_OPEN_DIR, "打开配置目录"),
        ] {
            let w = wide(text);
            AppendMenuW(menu, MF_STRING, id as usize, w.as_ptr());
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        let quit = wide("退出");
        AppendMenuW(menu, MF_STRING, ID_QUIT as usize, quit.as_ptr());

        // 不先抢到前台, 菜单在点击别处时不会消失
        SetForegroundWindow(hwnd);

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);

        // TPM_RETURNCMD: 直接返回选中的 ID, 不发 WM_COMMAND
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            0,
            hwnd,
            ptr::null(),
        );
        DestroyMenu(menu);

        if cmd > 0 {
            handle_command(cmd as u32);
        }
    }
}

fn handle_command(id: u32) {
    match id {
        ID_TOGGLE => {
            let on = !hook::is_enabled();
            hook::set_enabled(on);
            update_status();
            notify(
                "keyremap-ng",
                if on { "映射已启用" } else { "映射已禁用" },
            );
        }
        ID_RELOAD => {
            if let Some(path) = CONFIG_PATH.get() {
                match watcher::reload_now(path) {
                    Ok(n) => {
                        info!("已手动重载配置, {n} 条映射生效");
                        update_status();
                        notify("配置已重载", &format!("{n} 条映射生效"));
                    }
                    Err(e) => {
                        error!("{e}");
                        notify("配置重载失败", &e);
                    }
                }
            }
        }
        ID_OPEN_FILE => {
            if let Some(path) = CONFIG_PATH.get() {
                open_path(path);
            }
        }
        ID_OPEN_DIR => {
            if let Some(dir) = CONFIG_PATH.get().and_then(|p| p.parent()) {
                open_path(dir);
            }
        }
        ID_QUIT => {
            info!("退出");
            unsafe { PostQuitMessage(0) };
        }
        id if id >= ID_MAPPING_BASE => {
            let idx = (id - ID_MAPPING_BASE) as usize;
            if let Some(now) = hook::toggle_mapping(idx) {
                let name = hook::config_snapshot()
                    .and_then(|c| c.mappings.get(idx).map(|m| m.label()))
                    .unwrap_or_default();
                info!("映射 {name} 已{}", if now { "启用" } else { "禁用" });
                update_status();
            }
        }
        _ => {}
    }
}

// ---------- 辅助 ----------

/// 构造 NOTIFYICONDATAW 的公共部分。uFlags 由调用方按用途填。
fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_ICON_ID;
    data
}

/// 从 exe 内嵌资源加载图标 (资源名 "id" 定义在 assets/app.rc)。
/// 取不到就退回系统默认图标, 保证托盘一定有东西显示。
fn load_app_icon() -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let name = wide("id");
        let icon = LoadIconW(hinst, name.as_ptr());
        if icon.is_null() {
            warn!("加载内嵌图标失败, 使用系统默认图标");
            LoadIconW(ptr::null_mut(), IDI_APPLICATION)
        } else {
            icon
        }
    }
}

fn open_path(path: &Path) {
    let file = wide_os(path.as_os_str());
    let verb = wide("open");
    unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_os(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// 把字符串写进 NOTIFYICONDATAW 的定长字段, 超长截断并保证以 0 结尾。
fn fill_wide(dst: &mut [u16], s: &str) {
    let max = dst.len().saturating_sub(1);
    let src: Vec<u16> = s.encode_utf16().take(max).collect();
    dst[..src.len()].copy_from_slice(&src);
    dst[src.len()] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 定长字段写入并以零结尾() {
        let mut buf = [0xFFu16; 8];
        fill_wide(&mut buf, "ab");
        assert_eq!(buf[0], b'a' as u16);
        assert_eq!(buf[1], b'b' as u16);
        assert_eq!(buf[2], 0);
    }

    #[test]
    fn 超长内容被截断且不越界() {
        let mut buf = [0xFFu16; 4];
        fill_wide(&mut buf, "abcdefgh");
        assert_eq!(buf[3], 0, "最后一位必须是终止符");
        assert_eq!(&buf[..3], &[b'a' as u16, b'b' as u16, b'c' as u16]);
    }

    #[test]
    fn 映射项id可反解出下标() {
        for idx in [0usize, 1, 7, 42] {
            let id = ID_MAPPING_BASE + idx as u32;
            assert!(id >= ID_MAPPING_BASE);
            assert_eq!((id - ID_MAPPING_BASE) as usize, idx);
        }
        // 固定命令的 ID 不能落进映射区间
        for id in [ID_TOGGLE, ID_RELOAD, ID_OPEN_FILE, ID_OPEN_DIR, ID_QUIT] {
            assert!(id < ID_MAPPING_BASE);
        }
    }
}
