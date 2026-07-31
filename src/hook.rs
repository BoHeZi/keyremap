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
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, OnceLock, RwLock};

use log::{debug, info, warn};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_CAPITAL;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    XBUTTON1,
};

use crate::config::Config;
use crate::inject::{self, INJECTED_TAG};
use crate::keycode::{Input, MOD_CAPS, MouseButton, mods_held, name_from_mouse, name_from_vk};

/// 当前生效的配置。用 RwLock 包 Arc: 回调侧只读并克隆 Arc, 写侧 (热重载) 整体替换。
static CONFIG: OnceLock<RwLock<Arc<Config>>> = OnceLock::new();

/// 总开关。托盘菜单的"启用/禁用"改这个值。
static ENABLED: AtomicBool = AtomicBool::new(true);

/// 监听模式: 只打印不拦截, 用于让用户查出某个键叫什么名字。
static LISTEN_ONLY: AtomicBool = AtomicBool::new(false);

/// 监听模式的事件出口。回调只往这里投递, 格式化与打印都在另一个线程做。
///
/// **为什么不能在回调里直接打印**: 控制台一旦被鼠标选中 (快速编辑模式),
/// `WriteConsole` 会一直阻塞到选区被取消。而钩子回调阻塞意味着**全系统的输入**
/// 都在等它 —— 表现是整台机器卡死, 而不是只有本程序卡住。
/// 顺带也躲开了回调里的堆分配 (键名 `to_string`)。
static LISTEN_TX: OnceLock<SyncSender<(Input, bool)>> = OnceLock::new();

/// 队列满时丢弃的事件数。丢几条日志远好过卡住整个系统的输入。
static LISTEN_DROPPED: AtomicUsize = AtomicUsize::new(0);

/// 监听队列容量。手速再快也到不了这个量级, 留这么多是为了让打印侧
/// 被选区卡住几秒之后还能追上, 而不是立刻开始丢。
const LISTEN_QUEUE: usize = 4096;

/// 已安装的钩子句柄, 供退出时卸载。0 表示未安装。
static KEYBOARD_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);

// ---------- 对外状态接口 ----------

pub fn set_config(config: Config) {
    let cell = CONFIG.get_or_init(|| RwLock::new(Arc::new(Config::default())));
    match cell.write() {
        Ok(mut guard) => {
            // 闸门必须跟着一起换, 否则新加的映射会在第一道关卡就被放行,
            // 而删掉的映射对应的键还在白白往下走
            refresh_gate(&config);
            *guard = Arc::new(config);
        }
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
    update_config(|cfg| {
        let m = cfg.mappings.get_mut(index)?;
        m.enable = !m.enable;
        Some(m.enable)
    })
}

/// 切换整组的启用状态, 返回切换后的值。组关掉时组内所有映射一并停止生效。
pub fn toggle_group(index: usize) -> Option<bool> {
    update_config(|cfg| {
        let g = cfg.groups.get_mut(index)?;
        g.enable = !g.enable;
        Some(g.enable)
    })
}

/// 以 copy-on-write 的方式修改配置。
///
/// 整体替换 Arc 而不是原地改 —— 这样钩子回调侧永远看到一个完整一致的配置,
/// 不需要在热路径上加写锁。配置很小, 克隆的代价远低于让回调等锁。
fn update_config<T>(f: impl FnOnce(&mut Config) -> Option<T>) -> Option<T> {
    let cell = CONFIG.get()?;
    let mut guard = cell.write().ok()?;

    let mut next = (**guard).clone();
    let result = f(&mut next)?;

    *guard = Arc::new(next);
    Some(result)
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

/// 启动监听模式的打印线程。必须在装钩子之前调用, 否则最初几个事件没人接。
///
/// 把格式化和写控制台都挪到这个线程, 是为了让钩子回调无论如何都能立刻返回 ——
/// 见 [`LISTEN_TX`] 的说明。
pub fn start_listen_printer() {
    let (tx, rx) = sync_channel::<(Input, bool)>(LISTEN_QUEUE);
    if LISTEN_TX.set(tx).is_err() {
        return; // 已经启动过了
    }

    std::thread::spawn(move || {
        let mut reported = 0usize;
        for (input, is_down) in rx {
            let name = match input {
                Input::Key(vk) => name_from_vk(vk)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("未知键(VK=0x{vk:02X})")),
                Input::Mouse(btn) => name_from_mouse(btn).to_string(),
            };
            info!("{name}  [{}]", if is_down { "按下" } else { "抬起" });

            // 丢过事件就说一句。不说的话用户会以为自己没按到。
            let dropped = LISTEN_DROPPED.load(Ordering::Relaxed);
            if dropped > reported {
                warn!(
                    "输出跟不上, 已丢弃 {} 个事件 (终端窗口被鼠标选中会阻塞输出)",
                    dropped - reported
                );
                reported = dropped;
            }
        }
    });
}

// ---------- 匹配与处理 ----------

/// 监听模式下把事件投给打印线程。
///
/// 回调热路径: 一次原子读 + 一次不阻塞的投递, 不分配、不格式化、不碰 IO。
/// 队列满就丢并计数 —— 宁可少打一行日志, 也不能让回调等在这里。
fn report(input: Input, is_down: bool) {
    if !LISTEN_ONLY.load(Ordering::Relaxed) {
        return;
    }
    if let Some(tx) = LISTEN_TX.get()
        && let Err(TrySendError::Full(_)) = tx.try_send((input, is_down))
    {
        LISTEN_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// 查找并执行匹配的映射。返回 true 表示原事件应被吞掉。
fn dispatch(input: Input, is_down: bool) -> bool {
    report(input, is_down);

    if LISTEN_ONLY.load(Ordering::Relaxed) || !ENABLED.load(Ordering::Relaxed) {
        return false;
    }

    // 第一道闸门, 也是最要紧的一道: 这个输入源上根本没挂映射就直接放行。
    //
    // 只有一次数组读取 —— 不加锁, 也不克隆 Arc。绝大多数按键在这里就返回了,
    // 用户实际配了映射的键才值得往下走。位图在配置变更时重算, 见 refresh_gate。
    if !WATCHED[slot(input)].load(Ordering::Relaxed) {
        return false;
    }

    let Some(config) = current_config() else {
        return false;
    };

    // CapsLock 被当作修饰键用时要特殊照顾: 它本身按一下就切换大小写,
    // 直接放行的话按一次 CapsLock+H 会顺带把大小写切了。
    if config.uses_caps_mod && input == Input::Key(VK_CAPITAL) {
        return handle_caps(&config, is_down);
    }

    // 抬起事件优先看"这个键的按下是不是被我们吞掉的"。
    //
    // 不能只靠重新匹配一遍: 用户完全可能先松开 Ctrl 再松开 E, 那时修饰键
    // 条件已经不成立, 重新匹配会失败, 于是一个没有配对按下的 E 抬起漏给应用。
    if !is_down && take_triggered(input) {
        return true;
    }

    // 按 config.order 遍历: 修饰键多的排在前面, 这样 `Ctrl+E` 能盖过 `E`。
    for &i in &config.order {
        let Some(m) = config.mappings.get(i as usize) else {
            continue;
        };
        // 先比触发键 —— 一次整数比较, 绝大多数按键在这一步就被排除,
        // 根本不会去查修饰键状态。is_active 也只是一次数组索引。
        if m.from != input || !config.is_active(m) {
            continue;
        }
        // CapsLock 得单独问我们自己那份记录, 不能去问系统。
        //
        // 因为它的按下事件正是被我们吞掉的 —— 钩子回调返回非零之后事件就被丢弃,
        // 压根不会进入系统的按键状态, `GetAsyncKeyState(VK_CAPITAL)` 因此永远
        // 报告"没按下"。自己藏起来的键, 只能自己记着。
        if m.from_mods & MOD_CAPS != 0 && !CAPS_DOWN.load(Ordering::Relaxed) {
            continue;
        }
        if !mods_held(m.from_mods & !MOD_CAPS) {
            continue;
        }

        if m.has_mods() {
            if !is_down {
                // 带修饰键的映射只在按下时动作。抬起要么在上面被 take_triggered
                // 接走了, 要么是"按下时没匹配、抬起时才匹配上"的边缘情况 ——
                // 后者不该凭空补一个动作出来, 放行即可。
                continue;
            }
            // 用 CapsLock 触发过, 那这次按下就不算"单独轻点"了
            if m.from_mods & MOD_CAPS != 0 {
                CAPS_CONSUMED.store(true, Ordering::Relaxed);
            }

            // 长按时系统会重复发按下事件, 此时标记已经在了。
            let repeat = is_triggered(input);
            mark_triggered(input);

            // 单键目标允许连发 (长按 Ctrl+E 连续删字, 这正是这类映射的用处);
            // 组合键目标只认第一次 —— Ctrl+W 之类连发会连关一串标签页。
            if !m.is_block() && !(m.is_combo() && repeat) {
                inject::send_with_mods_released(m.from_mods, &m.to);
            }
        } else if m.is_block() {
            // 屏蔽: 按下抬起都吞掉, 什么也不发出
        } else if m.is_combo() {
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

// ---------- CapsLock 当修饰键 ----------

/// CapsLock 此刻是否按着 (由我们自己吞下的那些事件推出来)。
static CAPS_DOWN: AtomicBool = AtomicBool::new(false);
/// 本次按住期间有没有靠它触发过映射。抬起时用来区分"当修饰键用"和"单独轻点"。
static CAPS_CONSUMED: AtomicBool = AtomicBool::new(false);

/// 处理 CapsLock 自身的按下与抬起。返回 true 表示吞掉。
///
/// 按下一律吞掉 —— 放行的话大小写就被切了, 而用户按它是为了当修饰键。
///
/// 抬起时分两种情况:
///
/// - **期间触发过映射**: 它这次是在当修饰键, 吞掉收工。
/// - **单独轻点**: 不能就这么把 CapsLock 的原有功能吃掉。此时去看有没有为
///   CapsLock 单独配的映射 (不带修饰键的那种), 有就执行它, 没有就补发一次
///   真正的 CapsLock。
///
/// 这样三种用法用同一条规则就都成立了, 不需要额外的开关:
///
/// ```text
/// 只配 CapsLock+H          -> 轻点照常切换大小写, 什么都没丢
/// 再配 CapsLock -> []      -> 轻点什么都不做, 这才是"禁用 CapsLock"
/// 再配 CapsLock -> Esc     -> 轻点是 Esc, 按住是修饰键
/// ```
fn handle_caps(config: &Config, is_down: bool) -> bool {
    if is_down {
        // 长按会重复发按下事件, 只有第一次才重置"用过没有"
        if !CAPS_DOWN.swap(true, Ordering::Relaxed) {
            CAPS_CONSUMED.store(false, Ordering::Relaxed);
        }
        return true;
    }

    CAPS_DOWN.store(false, Ordering::Relaxed);
    if CAPS_CONSUMED.swap(false, Ordering::Relaxed) {
        return true;
    }

    // 单独轻点: 有没有专门给 CapsLock 配的映射?
    for &i in &config.order {
        let Some(m) = config.mappings.get(i as usize) else {
            continue;
        };
        if m.from == Input::Key(VK_CAPITAL) && !m.has_mods() && config.is_active(m) {
            if !m.is_block() {
                inject::send_combination(&m.to);
            }
            return true;
        }
    }

    // 没配就把它原样补一次, 免得平白剥夺了这个键本来的功能
    inject::send_combination(&[VK_CAPITAL]);
    true
}

// ---------- "按下已被吞掉"的记号 ----------

/// 每个可能的输入源一位, 记录它的按下事件是否被带修饰键的映射吞掉了。
///
/// 存在的意义只有一个: 保证抬起事件跟按下事件同进同出。少了它, 用户先松开
/// 修饰键再松开触发键时, 应用会收到一个没有配对按下的抬起 —— 有些程序会
/// 因此认为该键卡住了。
///
/// 用定长数组而不是 HashSet: 钩子回调里不能有堆分配, 也不该有哈希计算。
static TRIGGERED: [AtomicBool; TRIGGER_SLOTS] = [const { AtomicBool::new(false) }; TRIGGER_SLOTS];

/// 虚拟键 0..=255 各占一位, 之后接 5 个鼠标按键。
const TRIGGER_SLOTS: usize = 256 + 5;

/// 哪些输入源上挂着映射 —— 钩子回调的第一道闸门, 见 [`dispatch`]。
///
/// 单独摆一份位图而不是每次去遍历配置, 是因为遍历得先拿读锁再克隆 Arc,
/// 而**绝大多数按键跟配置毫无关系**, 那些代价完全是白花的。这里一次数组读取
/// 就能把它们放走。
///
/// 不看启用状态: 托盘里临时关掉一条映射不该动这份位图, 否则再打开时还得记得
/// 重算。启用与否交给后面的 `is_active`, 那已经不在热路径上了。
static WATCHED: [AtomicBool; TRIGGER_SLOTS] = [const { AtomicBool::new(false) }; TRIGGER_SLOTS];

/// 按新配置重算闸门位图。每次配置替换之后都要调用。
fn refresh_gate(config: &Config) {
    let mut next = [false; TRIGGER_SLOTS];
    for m in &config.mappings {
        next[slot(m.from)] = true;
    }
    // CapsLock 当修饰键时, 它自己的按下抬起也得进回调 —— 要吞掉
    if config.uses_caps_mod {
        next[slot(Input::Key(VK_CAPITAL))] = true;
    }

    for (cell, on) in WATCHED.iter().zip(next) {
        cell.store(on, Ordering::Relaxed);
    }
    // 顺手清掉遗留的"已吞下"记号: 配置一换, 记号对应的映射可能已经不在了,
    // 留着会让之后某个抬起事件被无端吞掉一次。
    for cell in &TRIGGERED {
        cell.store(false, Ordering::Relaxed);
    }
    CAPS_DOWN.store(false, Ordering::Relaxed);
    CAPS_CONSUMED.store(false, Ordering::Relaxed);
}

/// 输入源到位下标。虚拟键实际不会超过 255, 越界的一律折到最后兜底,
/// 宁可几个怪键共用一位, 也不能越界。
fn slot(input: Input) -> usize {
    match input {
        Input::Key(vk) => (vk as usize).min(255),
        Input::Mouse(btn) => 256 + btn as usize,
    }
}

fn mark_triggered(input: Input) {
    TRIGGERED[slot(input)].store(true, Ordering::Relaxed);
}

fn is_triggered(input: Input) -> bool {
    TRIGGERED[slot(input)].load(Ordering::Relaxed)
}

/// 读取并清除标记, 返回原值。
fn take_triggered(input: Input) -> bool {
    TRIGGERED[slot(input)].swap(false, Ordering::Relaxed)
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

/// 安装钩子。键盘钩子总是装, 鼠标钩子按当前配置决定 (没有鼠标映射时不装,
/// 省掉全部鼠标移动事件的回调开销)。
///
/// 要不要装鼠标钩子只从配置里读, **不接参数**。以前这里收一个 `with_mouse`,
/// 那等于把配置的一个瞬时快照变成了永久决定: 首次运行是空配置, 于是不装鼠标钩子,
/// 之后用户加了鼠标映射、热重载也只换了配置而没人去补装钩子, 鼠标映射就一直
/// 不生效 —— 而且不报错, 直到重启才好。
///
/// 调用前必须先 [`set_config`]。
pub fn install() -> Result<(), String> {
    let kh = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), ptr::null_mut(), 0) };
    if kh.is_null() {
        return Err("安装键盘钩子失败".into());
    }
    KEYBOARD_HOOK.store(kh as isize, Ordering::SeqCst);
    debug!("键盘钩子已安装");

    sync_mouse_hook()?;
    Ok(())
}

/// 让鼠标钩子的安装状态与当前配置一致, 返回是否发生了变化。启动和每次热重载后调用。
///
/// **必须在跑消息循环的那个线程上调用。** 低级钩子绑定在安装它的线程上, 由系统在
/// 该线程的消息派发中回调 —— 在文件监听线程上装出来的钩子永远不会被调用。
/// 所以热重载走的是"监听线程 PostThreadMessage → 主线程调用这里"这条路。
pub fn sync_mouse_hook() -> Result<bool, String> {
    // 监听模式要能报出鼠标侧键叫什么名字, 所以无条件装 —— 那种模式下
    // 根本没有配置可查 (它不读配置文件)。
    let listening = LISTEN_ONLY.load(Ordering::Relaxed);
    let needed = listening
        || current_config()
            .map(|c| c.needs_mouse_hook())
            .unwrap_or(false);
    let installed = MOUSE_HOOK.load(Ordering::SeqCst) != 0;

    if needed == installed {
        return Ok(false);
    }

    if needed {
        let mh = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), ptr::null_mut(), 0) };
        if mh.is_null() {
            return Err("安装鼠标钩子失败".into());
        }
        MOUSE_HOOK.store(mh as isize, Ordering::SeqCst);
        if listening {
            info!("已装上鼠标钩子 (监听模式要能报出鼠标键名)");
        } else {
            info!("配置里有鼠标映射, 已装上鼠标钩子");
        }
    } else {
        let h = MOUSE_HOOK.swap(0, Ordering::SeqCst);
        if h != 0 {
            unsafe { UnhookWindowsHookEx(h as *mut _) };
        }
        info!("配置里已无鼠标映射, 卸掉鼠标钩子");
    }
    Ok(true)
}

/// 鼠标钩子当前是否装着。托盘用它显示实际状态。
pub fn mouse_hook_active() -> bool {
    MOUSE_HOOK.load(Ordering::SeqCst) != 0
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

    #[test]
    fn 键盘与鼠标的槽位不重叠() {
        // 重叠的话, 按下 Ctrl+某个键之后再点鼠标, 会把别人的记号清掉,
        // 结果是漏吞一个抬起事件
        assert_eq!(slot(Input::Key(0)), 0);
        assert_eq!(slot(Input::Key(255)), 255);
        assert_eq!(slot(Input::Mouse(MouseButton::Left)), 256);
        assert!(slot(Input::Mouse(MouseButton::X2)) < TRIGGER_SLOTS);
    }

    #[test]
    fn 越界的虚拟键不会写到数组外() {
        // vkCode 来自系统结构体, 理论上不超过 255, 但越界写入的代价太大, 兜一下底
        assert!(slot(Input::Key(u16::MAX)) < TRIGGER_SLOTS);
    }

    #[test]
    fn 闸门只放行配了映射的输入源() {
        let cfg = crate::config::parse(
            "[[mappings]]\nfrom = \"Pause\"\nto = \"Insert\"\n\n\
             [[mappings]]\nfrom = \"MouseX2\"\nto = \"Delete\"",
        )
        .unwrap();
        refresh_gate(&cfg);

        let pause = crate::keycode::vk_from_name("Pause").unwrap();
        assert!(WATCHED[slot(Input::Key(pause))].load(Ordering::Relaxed));
        assert!(WATCHED[slot(Input::Mouse(MouseButton::X2))].load(Ordering::Relaxed));

        // 没配的键必须在第一道闸门就被放走 —— 那正是这份位图存在的意义
        let a = crate::keycode::vk_from_name("A").unwrap();
        assert!(!WATCHED[slot(Input::Key(a))].load(Ordering::Relaxed));
        assert!(!WATCHED[slot(Input::Mouse(MouseButton::Left))].load(Ordering::Relaxed));
        // 没人拿 CapsLock 当修饰键时, 它也不该被拦下来
        assert!(!WATCHED[slot(Input::Key(VK_CAPITAL))].load(Ordering::Relaxed));
    }

    #[test]
    fn 用了capslock修饰键才拦capslock() {
        let cfg = crate::config::parse("[[mappings]]\nfrom = [\"CapsLock\", \"H\"]\nto = \"Left\"")
            .unwrap();
        refresh_gate(&cfg);
        // 触发键是 H, 但 CapsLock 自身也必须进回调 —— 得吞掉它免得切大小写
        assert!(WATCHED[slot(Input::Key(VK_CAPITAL))].load(Ordering::Relaxed));
    }

    #[test]
    fn 换配置时清掉遗留的已吞下记号() {
        let k = Input::Key(201);
        mark_triggered(k);
        let cfg = crate::config::parse("[[mappings]]\nfrom = \"Pause\"\nto = \"Insert\"").unwrap();
        refresh_gate(&cfg);
        assert!(
            !is_triggered(k),
            "留着旧记号会让之后某个抬起事件被无端吞掉一次"
        );
    }

    #[test]
    fn 记号取一次就清掉() {
        let k = Input::Key(200);
        assert!(!is_triggered(k));
        mark_triggered(k);
        assert!(is_triggered(k));
        assert!(take_triggered(k), "第一次取应当拿到记号");
        assert!(!take_triggered(k), "第二次就没有了, 否则会连吞两个抬起");
    }
}
