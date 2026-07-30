//! 控制台按需附加。
//!
//! 程序编译为 GUI 子系统, 双击运行不会出现黑窗口。但 `--dump` / `--listen` /
//! `-v` 这些用法仍然需要能看到输出, 所以在需要时才去要一个控制台:
//!
//! - 从命令行启动: `AttachConsole(ATTACH_PARENT_PROCESS)` 附到调用方的终端
//! - 双击运行, 或从托盘菜单启动监听: 附不上, 那就 `AllocConsole` 自己开一个窗口
//!
//! 一个逻辑覆盖了三种场景, 不需要额外的参数去区分。
//!
//! 但**是附来的还是自己开的, 后续行为不一样**, 所以要记下来 (见 [`is_owned`]):
//! 自己开的窗口在进程退出的一瞬间就消失, 一次性输出的命令必须先停下来等一下,
//! 不然用户只看到一个黑框闪过。

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use log::debug;
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, ENABLE_EXTENDED_FLAGS,
    ENABLE_QUICK_EDIT_MODE, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode, SetStdHandle,
};

static READY: AtomicBool = AtomicBool::new(false);

/// 控制台的来源: 0 = 没有, 1 = 附到调用方的终端, 2 = 自己新开的窗口。
static ORIGIN: AtomicU8 = AtomicU8::new(ORIGIN_NONE);
const ORIGIN_NONE: u8 = 0;
const ORIGIN_ATTACHED: u8 = 1;
const ORIGIN_OWNED: u8 = 2;

/// 进程启动时 stdout 是否本来就被重定向到了文件或管道。
///
/// **必须在附加控制台之前记下来**: [`rebind_std_handles`] 之后所有标准句柄都变成
/// 有效的了, 再去看就分辨不出原本是管道还是空的。
static STDOUT_REDIRECTED: AtomicBool = AtomicBool::new(false);

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

    // 趁标准句柄还没被接管, 先记下 stdout 原本的去向
    STDOUT_REDIRECTED.store(
        unsafe { is_redirected(STD_OUTPUT_HANDLE) },
        Ordering::Relaxed,
    );

    // 注意区分两条路径。经 scoop 的 GUI shim 启动时, shim 不等子进程就退出,
    // 到这里父进程可能已经没了, AttachConsole 于是失败 —— 所以同一条命令
    // 有时附得上、有时只能自己开一个窗口, 表现出来就是"输出有时看不见"。
    let origin = unsafe {
        if !force_new && AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            ORIGIN_ATTACHED
        } else if AllocConsole() != 0 {
            ORIGIN_OWNED
        } else {
            ORIGIN_NONE
        }
    };

    if origin != ORIGIN_NONE {
        rebind_std_handles();
        ORIGIN.store(origin, Ordering::Relaxed);
        READY.store(true, Ordering::Relaxed);
    }
    origin != ORIGIN_NONE
}

/// 这个控制台是本进程自己开的窗口吗?
///
/// 是的话它会随进程退出一起消失, 一次性输出的命令得先 [`pause`] 一下。
pub fn is_owned() -> bool {
    ORIGIN.load(Ordering::Relaxed) == ORIGIN_OWNED
}

/// 自己开的控制台窗口在退出前停一下, 让用户读完输出。
///
/// 两个前提都得满足才停:
///
/// - **窗口是自己开的**。附到调用方终端时不用停, 那个窗口不会随本进程消失。
/// - **stdout 没有被重定向**。`keyremap --dump > out.txt` 这种是在采集输出,
///   不是在读窗口, 停下来只会把脚本挂死。判断用的是附加控制台**之前**记下的
///   状态, 因为之后所有句柄都变成有效的了, 分辨不出原本是管道还是空的。
pub fn pause_if_owned() {
    if !is_owned() || STDOUT_REDIRECTED.load(Ordering::Relaxed) {
        return;
    }
    println!("\n按回车关闭本窗口。(想把输出存成文件请用 -o <文件>)");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

/// 关掉控制台的**快速编辑模式**。
///
/// 开着的时候, 在窗口里点一下鼠标就进入选区状态, 而选区会让 `WriteConsole`
/// 一直阻塞到选区取消。对本程序来说这不只是输出停住: 监听模式下打印线程被卡住,
/// 队列填满之后事件开始丢弃 —— 关掉它就从根上不会发生。
///
/// 必须连 `ENABLE_EXTENDED_FLAGS` 一起给: 不带这个标志的话, 系统会忽略
/// 对快速编辑位的修改。
pub fn disable_quick_edit() {
    unsafe {
        let h = GetStdHandle(STD_INPUT_HANDLE);
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return;
        }
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) == 0 {
            return; // 不是真正的控制台 (被重定向了), 无所谓
        }
        let next = (mode & !ENABLE_QUICK_EDIT_MODE) | ENABLE_EXTENDED_FLAGS;
        if SetConsoleMode(h, next) == 0 {
            debug!("关闭快速编辑模式失败, 选中窗口仍会阻塞输出");
        }
    }
}

/// 把进程的标准句柄接到新控制台上。
///
/// AttachConsole / AllocConsole 之后, 进程原有的标准句柄仍指向旧目标 (通常无效),
/// 必须重新打开 CONOUT$ / CONIN$ 并设回去, 否则 println! 出来的东西看不见。
///
/// 但**只接管无效的句柄**: 已经被重定向到文件或管道的不能动, 否则
/// `keyremap --dump-keys > keys.json` 会写不进文件 —— 而 Web 配置工具
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
