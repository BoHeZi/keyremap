//! 权限检测与提权重启。
//!
//! 为什么需要管理员权限: Windows 的 UIPI 机制禁止低完整性级别的进程向高级别
//! 进程注入输入。所以当前台窗口属于一个管理员权限的程序时 (任务管理器、
//! 提权运行的编辑器等), 普通权限的 keyremap-ng 发出的 SendInput 会被丢弃 ——
//! 表现为"在某些窗口里映射突然不生效"。以管理员运行可以覆盖这些窗口。
//!
//! 注意: UAC 安全桌面和部分反作弊游戏无论如何都覆盖不到, 那需要内核级驱动。
//!
//! 与开机自启的关系: HKCU\Run 启动的进程不会提权。想要"开机自启 + 管理员"
//! 必须走任务计划程序 (以最高权限运行的任务), 本程序暂未实现, 需要的话
//! 可以手动在"任务计划程序"里建一个。

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;

use log::warn;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// 当前进程是否以管理员权限运行。
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);

        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// 以管理员身份重新启动自己。
///
/// 调用方必须**先释放单实例锁再调用**, 否则新实例会被旧实例的锁挡在门外。
/// 走 ShellExecuteW 的 "runas" 动词, 由系统弹 UAC 确认框;
/// 用户点取消时返回 false, 此时调用方应当继续正常运行而不是退出。
pub fn restart_as_admin(config_path: &std::path::Path) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        warn!("无法确定程序路径, 提权重启取消");
        return false;
    };

    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(&format!("-c \"{}\"", config_path.display()));

    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW 返回值 <= 32 表示失败, 最常见的是用户在 UAC 框点了取消
    let ok = result as isize > 32;
    if !ok {
        warn!("提权重启未执行 (可能是用户取消了 UAC 确认)");
    }
    ok
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
