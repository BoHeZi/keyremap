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
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use log::{debug, error, info, warn};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, GetSystemMetrics, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR,
    LoadIconW, LoadImageW, MB_ICONERROR, MB_OK, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, MessageBoxW, PostQuitMessage, RegisterClassW, SM_CXSMICON, SM_CYSMICON,
    SW_SHOWNORMAL, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
    WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
};

use crate::{autostart, elevate, hook, watcher};

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
const ID_LISTEN: u32 = 6;
const ID_AUTOSTART: u32 = 7;
const ID_RUNAS: u32 = 8;
const ID_MAPPING_BASE: u32 = 100;

/// 退出时是否要以管理员身份重启。
///
/// 提权不能原地进行, 必须重启进程。而新实例会被旧实例的单实例锁挡住,
/// 所以这里只置个标记, 由 main 在退出消息循环、释放锁之后再执行。
static RESTART_AS_ADMIN: AtomicBool = AtomicBool::new(false);

/// 取出并清除"提权重启"请求。
pub fn take_restart_request() -> bool {
    RESTART_AS_ADMIN.swap(false, Ordering::SeqCst)
}

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
        let label = wide("启用映射");
        AppendMenuW(
            menu,
            item_flags(hook::is_enabled()),
            ID_TOGGLE as usize,
            label.as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());

        // 映射开关收进子菜单
        append_mapping_submenu(menu);

        let listen = wide("按键监听...");
        AppendMenuW(menu, MF_STRING, ID_LISTEN as usize, listen.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());

        for (id, text) in [
            (ID_RELOAD, "重新加载配置"),
            (ID_OPEN_FILE, "打开配置文件"),
            (ID_OPEN_DIR, "打开配置目录"),
        ] {
            let w = wide(text);
            AppendMenuW(menu, MF_STRING, id as usize, w.as_ptr());
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());

        let autorun = wide("开机自启动");
        AppendMenuW(
            menu,
            item_flags(autostart::is_enabled()),
            ID_AUTOSTART as usize,
            autorun.as_ptr(),
        );

        // 已经是管理员时没什么可做的, 显示成灰色状态项
        if elevate::is_elevated() {
            let t = wide("已以管理员身份运行");
            AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, t.as_ptr());
        } else {
            let t = wide("以管理员身份重启");
            AppendMenuW(menu, MF_STRING, ID_RUNAS as usize, t.as_ptr());
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

/// 菜单项的标志位: 勾选与否。
fn item_flags(checked: bool) -> u32 {
    if checked {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    }
}

/// 把逐条映射的开关挂成子菜单。
///
/// 映射条数是随配置增长的, 全摊在一级菜单里会让菜单越来越长, 而且"退出"这类
/// 常用项的位置会随配置数量上下漂移。收进子菜单后一级菜单的结构就固定了。
unsafe fn append_mapping_submenu(parent: HMENU) {
    let cfg = hook::config_snapshot();
    let mappings = cfg.as_ref().map(|c| c.mappings.as_slice()).unwrap_or(&[]);

    if mappings.is_empty() {
        let text = wide("映射开关 (无配置)");
        unsafe { AppendMenuW(parent, MF_STRING | MF_GRAYED, 0, text.as_ptr()) };
        return;
    }

    let sub = unsafe { CreatePopupMenu() };
    if sub.is_null() {
        warn!("创建子菜单失败");
        return;
    }

    unsafe {
        for (i, m) in mappings.iter().enumerate() {
            let w = wide(&m.label());
            AppendMenuW(
                sub,
                item_flags(m.enable),
                (ID_MAPPING_BASE + i as u32) as usize,
                w.as_ptr(),
            );
        }

        let on = mappings.iter().filter(|m| m.enable).count();
        let title = wide(&format!("映射开关 ({on}/{})", mappings.len()));
        // MF_POPUP 时第三个参数是子菜单句柄而非命令 ID。
        // 父菜单 DestroyMenu 时会连带销毁子菜单, 不用单独释放。
        AppendMenuW(parent, MF_POPUP | MF_STRING, sub as usize, title.as_ptr());
    }
}

/// 另起一个进程跑监听模式。
///
/// 监听模式不受单实例限制, 所以可以和正在运行的实例并存。传 --new-console
/// 是为了让它自己开一个终端窗口, 而不是附到本进程可能存在的控制台上。
fn spawn_listen() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!("无法定位自身路径: {e}");
            return;
        }
    };
    let file = wide_os(exe.as_os_str());
    let params = wide("--listen --new-console");
    let verb = wide("open");
    unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            ptr::null(),
            SW_SHOWNORMAL,
        );
    }
    info!("已启动按键监听窗口");
}

fn toggle_autostart() {
    let turn_on = !autostart::is_enabled();
    let result = match CONFIG_PATH.get() {
        Some(path) if turn_on => autostart::enable(path),
        Some(_) => autostart::disable(),
        None => Err("配置路径未知".to_string()),
    };

    match result {
        Ok(()) => {
            let msg = if turn_on {
                "已设置为开机自启动"
            } else {
                "已取消开机自启动"
            };
            info!("{msg}");
            notify("keyremap-ng", msg);
        }
        Err(e) => {
            error!("设置自启动失败: {e}");
            notify("设置自启动失败", &e);
        }
    }
}

/// 托盘还没建立起来时报错用。GUI 子系统下没有控制台, 错误信息会彻底消失,
/// 所以用消息框兜底 —— 启动失败必须让用户看得见。
pub fn show_error(title: &str, message: &str) {
    let t = wide(title);
    let m = wide(message);
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            m.as_ptr(),
            t.as_ptr(),
            MB_ICONERROR | MB_OK,
        );
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
                if on {
                    "映射已启用"
                } else {
                    "映射已禁用"
                },
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
        ID_LISTEN => spawn_listen(),
        ID_AUTOSTART => toggle_autostart(),
        ID_RUNAS => {
            // 只置标记, 真正的重启在 main 里做 —— 那时单实例锁已经释放
            RESTART_AS_ADMIN.store(true, Ordering::SeqCst);
            unsafe { PostQuitMessage(0) };
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

/// 从 exe 内嵌资源加载托盘图标 (资源名 "id" 定义在 assets/app.rc)。
///
/// 关键是用 `LoadImageW` 并显式指定**小图标**尺寸, 而不是 `LoadIconW` ——
/// 后者总是加载大图标 (SM_CXICON, 通常 32x32), 托盘再缩到 16x16 显示,
/// 结果就是发虚。指定尺寸后系统会从 ICO 里挑最接近的那一张
/// (本项目的图标含 16/24/32/48 四种), 无需缩放。
///
/// 尺寸取自 `SM_CXSMICON`, 它在声明了 DPI 感知后会返回按当前缩放换算的真实像素,
/// 所以高 DPI 屏上也能拿到清晰的图标。
fn load_app_icon() -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let name = wide("id");
        let cx = GetSystemMetrics(SM_CXSMICON);
        let cy = GetSystemMetrics(SM_CYSMICON);

        let icon = LoadImageW(hinst, name.as_ptr(), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR);
        if !icon.is_null() {
            return icon as HICON;
        }

        warn!("加载内嵌图标失败, 使用系统默认图标");
        LoadIconW(ptr::null_mut(), IDI_APPLICATION)
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
