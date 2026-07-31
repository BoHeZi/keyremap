//! 前台窗口所属进程的跟踪。
//!
//! 存在的理由是让映射能限定在某些程序里生效 (`window = "chrome.exe"`)。
//!
//! # 为什么不在钩子回调里查进程
//!
//! 天真的做法是回调里 `GetForegroundWindow` → `OpenProcess` →
//! `QueryFullProcessImageNameW` → 字符串比较。这在**每一次按键**上都要打开一个
//! 内核对象再做若干次字符串比较, 而钩子回调是全系统按键的必经之路 ——
//! 那正是这个项目一直在极力避免的事 (见 `hook::WATCHED` 的说明)。
//!
//! # 实际做法: 把判断挪到前台切换的那一刻
//!
//! 前台窗口切换是**人手速度**的事件, 一秒钟撑死几次; 按键则是每秒几十次。
//! 所以昂贵的查询只在切换时做一次:
//!
//! ```text
//! 前台切换 (SetWinEventHook)  ->  查进程名 -> 逐条比对配置里的窗口规则
//!                             ->  把"此刻成立的规则"存成一个 u64 位掩码
//!
//! 按键 (钩子回调)              ->  一次原子读 + 一次 & 运算
//! ```
//!
//! 于是热路径上既没有字符串, 也没有系统调用, 只剩两条整数指令。
//!
//! # 为什么必须装在主线程
//!
//! `WINEVENT_OUTOFCONTEXT` 的回调是由系统投递到**安装它的那个线程**的消息队列、
//! 再由消息泵派发的 —— 和低级钩子的机制完全一样。装在别的线程上就永远不会被调用。
//! 本程序所有东西都跑在同一个消息循环上, 正好合用。
//!
//! 代价是这个回调与钩子回调**共享线程**: 回调里做慢操作会连累按键。所以这里只
//! 做一次 `OpenProcess` + 一次路径查询 (几十微秒量级), 绝不碰 IO 和锁竞争。

use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use log::debug;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, MAX_PATH};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetWindowThreadProcessId, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};

use crate::config::Config;

/// 窗口规则的数量上限。一位一条, 所以受 u64 的宽度所限。
///
/// 64 条不同的规则已经远超实际用量 —— 相同写法的规则会合并成同一条 (见
/// [`crate::config`] 的 `intern_window_rule`), 所以这个数字指的是**互不相同**的
/// 窗口条件个数。
pub const MAX_WINDOW_RULES: usize = 64;

/// 当前前台进程满足哪些窗口规则, 一位一条。
///
/// 这是本模块存在的全部意义: 把"当前是哪个程序"这个字符串问题, 预先算成一个
/// 钩子回调可以用一条 `&` 指令回答的整数问题。
static ACTIVE: AtomicU64 = AtomicU64::new(0);

/// 已安装的 WinEvent 钩子句柄, 供退出时卸载。0 表示未安装。
static EVENT_HOOK: AtomicIsize = AtomicIsize::new(0);

/// 当前生效的规则表。只在前台切换与配置重载时读写, 不在钩子热路径上。
static RULES: OnceLock<Mutex<Vec<WindowRule>>> = OnceLock::new();

/// 最近一次解析出的前台进程名 (小写)。仅供 `--dump` 和日志使用。
static CURRENT: OnceLock<Mutex<String>> = OnceLock::new();

/// 一条编译好的窗口规则。
///
/// 语义: **有肯定项时必须命中其一, 且不能命中任何否定项。**
/// 全是否定项就表示"除这些之外都行"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowRule {
    /// 必须是其中之一。空表示不限制。进程名, 已转小写。
    pub include: Vec<String>,
    /// 不能是其中任何一个。进程名, 已转小写。
    pub exclude: Vec<String>,
    /// 用户写的原样, 用于菜单显示和 `--dump`。
    pub label: String,
}

impl WindowRule {
    /// 这条规则对给定的进程名是否成立。
    ///
    /// `process` 为空表示进程名拿不到 (极少数提权或系统进程)。此时肯定项一律
    /// 不成立、否定项一律成立 —— 也就是"限定在某程序里"的映射会安静地不生效,
    /// 而"除某程序之外"的映射照常工作。这个降级方向是刻意选的: 宁可少生效一次,
    /// 也不要在一个我们根本没认出来的程序里凭空触发按键。
    pub fn matches(&self, process: &str) -> bool {
        if !self.include.is_empty() && !self.include.iter().any(|p| p == process) {
            return false;
        }
        !self.exclude.iter().any(|p| p == process)
    }
}

/// 钩子回调读取的当前状态。一次 relaxed 原子读, 别的什么都不做。
#[inline]
pub fn active_rules() -> u64 {
    ACTIVE.load(Ordering::Relaxed)
}

/// 按新配置换掉规则表, 并立即重算一次当前状态。
///
/// 必须在每次配置替换后调用 —— 位掩码里的每一位对应规则表里的一条, 配置换了
/// 而位掩码没重算的话, 位的含义就和映射对不上了, 表现为映射在错误的程序里生效。
pub fn set_rules(config: &Config) {
    if let Some(m) = RULES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .ok()
        .as_deref_mut()
    {
        m.clear();
        m.extend_from_slice(&config.window_rules);
    }
    refresh();
}

/// 重新识别前台进程并重算位掩码。
///
/// 前台切换、配置重载、以及启动时各调用一次。
fn refresh() {
    let hwnd = unsafe { GetForegroundWindow() };
    let process = process_name(hwnd);

    let mask = match RULES.get().and_then(|m| m.lock().ok()) {
        Some(rules) => {
            let mut mask = 0u64;
            for (i, r) in rules.iter().enumerate() {
                if r.matches(&process) {
                    mask |= 1u64 << i;
                }
            }
            mask
        }
        None => 0,
    };

    // 先存名字再存掩码的顺序无关紧要: 名字只给人看, 掩码才是热路径读的。
    let changed = match CURRENT
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .ok()
        .as_deref_mut()
    {
        Some(m) if *m != process => {
            m.clear();
            m.push_str(&process);
            true
        }
        _ => false,
    };

    ACTIVE.store(mask, Ordering::Relaxed);

    // 只在真的换了程序时说话。同一个程序内部切窗口也会触发前台事件,
    // 每次都打一行的话监听模式下会刷屏。
    if changed {
        debug!(
            "前台程序: {}",
            if process.is_empty() {
                "(识别不出来)"
            } else {
                &process
            }
        );
        crate::hook::report_foreground(process);
    }
}

/// 取窗口所属进程的可执行文件名 (小写, 不含路径)。拿不到时返回空串。
///
/// 用 `PROCESS_QUERY_LIMITED_INFORMATION` 而不是 `PROCESS_QUERY_INFORMATION`:
/// 前者是 Vista 专门为"只想知道是哪个程序"这类需求加的, 权限要求低得多,
/// 普通权限进程也能查到同一用户下提权进程的路径。跨用户的系统进程仍然会失败,
/// 那时返回空串, 由 [`WindowRule::matches`] 决定降级行为。
fn process_name(hwnd: HWND) -> String {
    if hwnd.is_null() {
        return String::new();
    }

    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return String::new();
    }

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return String::new();
    }

    let mut buf = [0u16; MAX_PATH as usize];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) };
    unsafe { CloseHandle(handle) };

    if ok == 0 || len == 0 {
        return String::new();
    }

    let full = String::from_utf16_lossy(&buf[..len as usize]);
    file_name_lower(&full)
}

/// 从完整路径里取出文件名并转小写。
///
/// 单独拆出来是为了能直接测 —— 路径拆分的边界情况 (结尾是分隔符、只有文件名、
/// 混用正反斜杠) 比看上去多。
fn file_name_lower(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_lowercase()
}

// ---------- 安装 / 卸载 ----------

/// 让 WinEvent 钩子的安装状态与当前配置一致, 返回是否发生了变化。
///
/// 没有窗口规则时不装 —— 装上就意味着系统在每次前台切换时都要来叫我们一次,
/// 用不着就别占这份开销。
///
/// 和 `hook::sync_mouse_hook` 是同一个套路, 连"不接参数、自己去读当前配置"
/// 都一样: 接一个 config 参数就等于把某个瞬时快照变成永久决定, 那正是鼠标钩子
/// 当初出过的那个 bug (配置热重载后新增的鼠标映射一直不生效)。
///
/// **必须在跑消息循环的那个线程上调用**, 理由见模块开头。
pub fn sync() -> Result<bool, String> {
    // 监听模式要能告诉用户"当前前台程序叫什么名字", 所以无条件跟踪 ——
    // 那种模式下根本没有配置可查 (它不读配置文件)。
    let listening = crate::hook::is_listen_only();
    let needed = listening
        || crate::hook::config_snapshot()
            .map(|c| c.uses_window)
            .unwrap_or(false);
    let installed = EVENT_HOOK.load(Ordering::SeqCst) != 0;
    if needed == installed {
        return Ok(false);
    }

    if needed {
        let h = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                std::ptr::null_mut(),
                Some(win_event_proc),
                0, // 全部进程
                0, // 全部线程
                // OUTOFCONTEXT: 不把我们的 DLL 注入到别的进程里, 回调改为投递到
                // 本线程的消息队列。SKIPOWNPROCESS: 自己的托盘窗口不必通知。
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if h.is_null() {
            return Err("安装前台窗口监视失败, 按程序区分的映射将不生效".into());
        }
        EVENT_HOOK.store(h as isize, Ordering::SeqCst);
        // 装好之后立刻认一次当前前台: 不然要等到用户下次切窗口才开始生效
        refresh();
        if listening {
            debug!("已开始跟踪前台窗口 (监听模式要报出程序名)");
        } else {
            debug!("配置里有按程序限定的映射, 已开始跟踪前台窗口");
        }
    } else {
        let h = EVENT_HOOK.swap(0, Ordering::SeqCst);
        if h != 0 {
            unsafe { UnhookWinEvent(h as *mut _) };
        }
        // 不再跟踪就把掩码清零。留着旧值会让刚被改成"不限窗口"的映射
        // 继续按上一次的前台状态判断。
        ACTIVE.store(0, Ordering::Relaxed);
        debug!("配置里已无按程序限定的映射, 停止跟踪前台窗口");
    }
    Ok(true)
}

pub fn uninstall() {
    let h = EVENT_HOOK.swap(0, Ordering::SeqCst);
    if h != 0 {
        unsafe { UnhookWinEvent(h as *mut _) };
    }
}

/// 前台窗口变化的回调。
///
/// 由系统在本线程的消息派发中调用, 与钩子回调共享线程, 所以这里同样不能慢。
/// 实际工作只有一次 `OpenProcess` + 一次路径查询 + 几十次短字符串比较。
unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // 刻意不用回调带来的 hwnd, 而是重新问一次 GetForegroundWindow:
    // EVENT_SYSTEM_FOREGROUND 可能在窗口真正拿到前台之前就到达, 也可能连着来
    // 好几个。以系统当下的答案为准, 结果总是自洽的。
    refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(include: &[&str], exclude: &[&str]) -> WindowRule {
        WindowRule {
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            label: String::new(),
        }
    }

    #[test]
    fn 只有肯定项时必须命中其一() {
        let r = rule(&["chrome.exe", "msedge.exe"], &[]);
        assert!(r.matches("chrome.exe"));
        assert!(r.matches("msedge.exe"));
        assert!(!r.matches("code.exe"));
    }

    #[test]
    fn 只有否定项时除它们之外都成立() {
        let r = rule(&[], &["code.exe"]);
        assert!(r.matches("chrome.exe"));
        assert!(!r.matches("code.exe"));
    }

    #[test]
    fn 肯定与否定同时存在时两个条件都要满足() {
        let r = rule(&["chrome.exe", "code.exe"], &["code.exe"]);
        assert!(r.matches("chrome.exe"));
        assert!(!r.matches("code.exe"), "否定项应当盖过肯定项");
    }

    #[test]
    fn 进程名未知时肯定项不成立而否定项成立() {
        // 这是刻意选的降级方向: 认不出来的程序里, 宁可少生效, 也不要凭空触发
        assert!(!rule(&["chrome.exe"], &[]).matches(""));
        assert!(rule(&[], &["code.exe"]).matches(""));
    }

    #[test]
    fn 空规则对任何进程都成立() {
        assert!(rule(&[], &[]).matches("anything.exe"));
        assert!(rule(&[], &[]).matches(""));
    }

    #[test]
    fn 取文件名并转小写() {
        assert_eq!(
            file_name_lower(r"C:\Program Files\Chrome\CHROME.EXE"),
            "chrome.exe"
        );
        assert_eq!(file_name_lower("/usr/bin/Foo"), "foo");
        assert_eq!(file_name_lower("bare.exe"), "bare.exe");
    }

    #[test]
    fn 路径边界情况不会panic() {
        assert_eq!(file_name_lower(""), "");
        assert_eq!(file_name_lower(r"C:\dir\"), "dir");
        assert_eq!(file_name_lower(r"\\"), "");
    }
}
