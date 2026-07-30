//! 控制台按需附加。
//!
//! 程序编译为 GUI 子系统, 双击运行不会出现黑窗口。但 `--dump` / `--listen` /
//! `-v` 这些用法仍然需要能看到输出, 所以在需要时才去要一个控制台:
//!
//! - 从命令行启动: `AttachConsole(ATTACH_PARENT_PROCESS)` 附到调用方的终端
//! - 双击运行, 或从托盘菜单启动监听: 附不上, 那就 `AllocConsole` 自己开一个窗口
//!
//! 一个逻辑覆盖了三种场景, 不需要额外的参数去区分。

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};

static READY: AtomicBool = AtomicBool::new(false);

/// 确保当前进程有一个可用的控制台: 优先附到调用方的终端, 附不上就自己开一个。
///
/// 必须在任何输出之前调用: Rust 的标准流句柄是惰性初始化的, 一旦初始化过
/// 就不会再看 `SetStdHandle` 的结果。
pub fn ensure() -> bool {
    ensure_impl(false)
}

/// 强制新开一个控制台窗口, 不复用调用方的。
/// 从托盘菜单启动监听时用: 那种场景下要的是一个独立窗口, 而不是把输出
/// 混进主实例可能附着的终端里。
pub fn ensure_new() -> bool {
    ensure_impl(true)
}

fn ensure_impl(force_new: bool) -> bool {
    if READY.load(Ordering::Relaxed) {
        return true;
    }

    let ok = unsafe {
        if force_new {
            AllocConsole() != 0
        } else {
            AttachConsole(ATTACH_PARENT_PROCESS) != 0 || AllocConsole() != 0
        }
    };
    if ok {
        rebind_std_handles();
        READY.store(true, Ordering::Relaxed);
    }
    ok
}

/// 把进程的标准句柄接到新控制台上。
///
/// AttachConsole / AllocConsole 之后, 进程原有的标准句柄仍指向旧目标 (通常无效),
/// 必须重新打开 CONOUT$ / CONIN$ 并设回去, 否则 println! 出来的东西看不见。
///
/// 但**只接管无效的句柄**: 已经被重定向到文件或管道的不能动, 否则
/// `keyremap-ng --dump-keys > keys.json` 会写不进文件 —— 而 Web 配置工具
/// 正是靠这条命令取键名表的。
fn rebind_std_handles() {
    unsafe {
        let mut console_out = INVALID_HANDLE_VALUE;

        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if is_redirected(which) {
                continue;
            }
            if console_out == INVALID_HANDLE_VALUE {
                console_out = open_console_device("CONOUT$", GENERIC_READ | GENERIC_WRITE);
            }
            if console_out != INVALID_HANDLE_VALUE {
                SetStdHandle(which, console_out);
            }
        }

        if !is_redirected(STD_INPUT_HANDLE) {
            let input = open_console_device("CONIN$", GENERIC_READ | GENERIC_WRITE);
            if input != INVALID_HANDLE_VALUE {
                SetStdHandle(STD_INPUT_HANDLE, input);
            }
        }
    }
}

/// 该标准句柄是否已经指向有效目标 (被重定向到文件或管道)。
///
/// GUI 子系统的进程在没有重定向时, GetStdHandle 返回 null。
unsafe fn is_redirected(which: STD_HANDLE) -> bool {
    let h = unsafe { GetStdHandle(which) };
    !h.is_null() && h != INVALID_HANDLE_VALUE
}

unsafe fn open_console_device(name: &str, access: u32) -> HANDLE {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    }
}
