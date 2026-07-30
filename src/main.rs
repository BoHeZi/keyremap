// 编译为 GUI 子系统: 双击运行不弹黑窗口。需要输出时再按需附加控制台,
// 见 console 模块。
#![windows_subsystem = "windows"]

//! keyremap: Windows 键盘/鼠标重映射。
//!
//! 全部功能都跑在一个线程上: 低级钩子的回调、托盘窗口消息、配置重载通知
//! 共用同一个消息循环。低级钩子本来就需要消息泵, 托盘图标也需要窗口,
//! 两者天然可以合并。

mod autostart;
mod config;
mod console;
mod elevate;
mod hook;
mod inject;
mod keycode;
mod regutil;
mod singleton;
mod tray;
mod watcher;

use std::path::PathBuf;
use std::ptr;

use clap::Parser;
use log::{LevelFilter, error, info, warn};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, TranslateMessage,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "Windows 键盘/鼠标重映射工具", long_about = None)]
struct Args {
    /// 配置文件路径, 默认为可执行文件同目录下的 keyremap.toml
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// 提高日志级别, 可重复: -v, -vv。会自动附加一个控制台来显示日志
    #[arg(short, action = clap::ArgAction::Count)]
    verbose: u8,

    /// 监听模式: 只打印按键名, 不做任何映射。用于查某个键叫什么
    #[arg(short, long)]
    listen: bool,

    /// 打印已加载的映射后退出
    #[arg(long)]
    dump: bool,

    /// 输出键名表 (JSON) 后退出, 供 Web 配置工具消费
    #[arg(long)]
    dump_keys: bool,

    /// 把日志写入 exe 同目录的 keyremap.log
    #[arg(long)]
    logfile: bool,

    /// 与 --dump / --dump-keys 配合: 输出写入文件而不是控制台。
    /// GUI 程序的 shell 重定向不可靠 (shell 不等待进程结束), 要落盘就用这个
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// 内部使用: 强制新开控制台窗口, 而不是附到调用方的终端。
    /// 托盘菜单启动"按键监听"时会带上它。
    #[arg(long, hide = true)]
    new_console: bool,
}

fn main() {
    let args = Args::parse();

    // 必须在创建任何窗口之前声明 DPI 感知, 否则系统会对窗口做位图拉伸,
    // 菜单文字和托盘图标都会发虚。
    enable_dpi_awareness();

    // 只在真的要输出时才去要控制台。--dump 之类走 println 必须有控制台;
    // 日志类输出如果已经写文件就不必再开窗口。
    // 指定了 -o 就写文件, 不需要控制台
    let needs_stdout = (args.dump || args.dump_keys) && args.output.is_none();
    let needs_log_console = (args.listen || args.verbose > 0) && !args.logfile;
    if needs_stdout || needs_log_console {
        if args.new_console {
            console::ensure_new();
        } else {
            console::ensure();
        }
    }

    init_logger(&args);

    if args.dump_keys {
        emit(&keys_json(), args.output.as_deref());
        return;
    }

    // 监听模式不需要配置文件, 也不受单实例限制 ——
    // 它只读不拦截, 正常实例运行时也该能用它查键名。
    if args.listen {
        info!("=== 监听模式: 按键只显示不映射 ===");
        info!("按 Ctrl+C 或关闭本窗口退出\n");
        hook::set_listen_only(true);
        if let Err(e) = hook::install(true) {
            error!("{e}");
            return;
        }
        hook::run_message_loop();
        hook::uninstall();
        return;
    }

    // 规范成绝对路径。相对路径的 parent() 是空串而不是 None,
    // 会让文件监听拿不到可用的目录, 热重载直接失效。
    // 用 absolute 而非 canonicalize: 后者要求文件已存在, 且会产生 \\?\ 形式的 UNC 路径。
    let config_path = args.config.unwrap_or_else(default_config_path);
    let config_path = std::path::absolute(&config_path).unwrap_or(config_path);
    info!("加载配置: {}", config_path.display());

    let cfg = match config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            // GUI 子系统下没有控制台时错误会彻底消失, 用消息框兜底
            tray::show_error("keyremap: 配置加载失败", &e);
            std::process::exit(1);
        }
    };

    // 输入源撞车的话在这里就说出来, 别等用户按了半天发现某条从来没生效
    config::warn_conflicts(&cfg);

    if args.dump {
        emit(&dump_text(&cfg), args.output.as_deref());
        return;
    }

    // 用户要求以管理员身份运行、而当前不是的话, 换一个提权的进程接手。
    //
    // 必须放在拿单实例锁**之前**: 否则本进程持锁期间新进程会被挡在门外,
    // 而本进程又在等新进程起来, 变成死结。
    if elevate::wants_admin() && !elevate::is_elevated() {
        if elevate::restart_as_admin(&config_path) {
            info!("已交给管理员权限的新实例, 本进程退出");
            return;
        }
        // UAC 被取消时降级继续跑, 而不是干脆不启动 —— 后者更糟。
        // 提权失败不会循环: 新进程起不来, 这里就直接往下走了。
        warn!("提权未成功, 以普通权限继续运行 (对管理员权限的窗口将不生效)");
    }

    // 单实例检查放在装钩子之前。多个实例各装一套低级钩子会互相干扰:
    // 事件被逐层处理, 表现为"禁用了却还在生效"这类难以排查的现象。
    //
    // 锁的粒度是 exe 路径而非全局: 同一个副本只能跑一个, 但复制到别处的
    // 另一份可以带着各自的配置独立运行。
    let _instance = match singleton::SingleInstance::acquire(&singleton::name_for_current_exe()) {
        Some(i) => i,
        None => {
            let msg = "这个位置的 keyremap 已经在运行了";
            error!("{msg}, 本次启动取消");
            tray::show_error("keyremap", msg);
            std::process::exit(1);
        }
    };

    let with_mouse = cfg.needs_mouse_hook();
    info!(
        "已启用 {} 条映射{}",
        cfg.active_count(),
        if with_mouse { " (含鼠标)" } else { "" }
    );
    for m in cfg.mappings.iter().filter(|m| m.enable) {
        info!("  {} : {}", m.name, m);
    }

    hook::set_config(cfg);
    if let Err(e) = hook::install(with_mouse) {
        error!("{e}");
        std::process::exit(1);
    }

    // 让自启机制与"以管理员启动"偏好保持一致。
    // 用户刚打开该偏好时还没有权限建计划任务, 提权重启后由这里补上,
    // 并把旧的 Run 项清掉。幂等, 已经一致时不做任何写入。
    if let Err(e) = autostart::sync(&config_path) {
        warn!("自启机制同步失败: {e}");
    }

    // 文件监听要在托盘之前起来, 这样启动后立刻改配置也不会漏掉。
    // Debouncer 必须持有到程序结束, drop 掉监听就停了。
    let main_thread = unsafe { GetCurrentThreadId() };
    let _watcher = match watcher::spawn(&config_path, main_thread) {
        Ok(w) => Some(w),
        Err(e) => {
            // 监听失败不致命: 大不了退回手动重载
            warn!("{e}; 配置热重载不可用");
            None
        }
    };

    if let Err(e) = tray::init(&config_path) {
        error!("{e}");
        hook::uninstall();
        std::process::exit(1);
    }

    info!("=== 运行中, 通过托盘菜单退出 ===");

    run_app_loop();
    tray::shutdown();
    hook::uninstall();

    // 提权重启必须放在释放单实例锁之后, 否则新实例会被自己的旧锁挡在门外。
    if tray::take_restart_request() {
        drop(_instance);
        info!("正在以管理员身份重启");
        elevate::restart_as_admin(&config_path);
    }
}

/// 按组列出配置。
///
/// 状态列反映的是**实际是否生效**而不只是映射自身的开关: 一条自身启用、
/// 但所属组被关掉的映射并不工作, 标成 on 会造成误导。
fn dump_text(cfg: &config::Config) -> String {
    let mut text = format!("配置名称: {}\n", cfg.name);
    text.push_str(&format!(
        "生效 {}/{} 条\n",
        cfg.active_count(),
        cfg.mappings.len()
    ));

    for (gi, g) in cfg.groups.iter().enumerate() {
        let (on, total) = cfg.group_counts(gi);
        text.push_str(&format!(
            "\n【{}】  {on}/{total}{}\n",
            g.display_name(),
            if g.enable { "" } else { "  (整组已禁用)" }
        ));

        for (_, m) in cfg.mappings_in_group(gi) {
            let state = if cfg.is_active(m) {
                "on "
            } else if !m.enable {
                "off"
            } else {
                // 自身开着但组关着
                "grp"
            };
            text.push_str(&format!("  [{state}] {:<20} {m}\n", m.name));
            if !m.comment.is_empty() {
                text.push_str(&format!("        {}\n", m.comment));
            }
        }
    }
    let conflicts = cfg.find_conflicts();
    if !conflicts.is_empty() {
        text.push_str("\n冲突 (先到先得, 被遮盖的那条不会生效):\n");
        for (first, shadowed) in conflicts {
            text.push_str(&format!(
                "  {} 被「{}」占用, 「{}」不生效\n",
                cfg.mappings[first].input_name(),
                cfg.mappings[first].name,
                cfg.mappings[shadowed].name,
            ));
        }
    }

    text.push_str("\n状态: on=生效  off=该条已关闭  grp=所属组已关闭");
    text
}

/// 把文本输出到文件或控制台。
fn emit(text: &str, output: Option<&std::path::Path>) {
    match output {
        Some(path) => match std::fs::write(path, text) {
            Ok(()) => info!("已写入 {}", path.display()),
            Err(e) => error!("写入 {} 失败: {e}", path.display()),
        },
        None => println!("{text}"),
    }
}

/// 声明 per-monitor DPI 感知。
///
/// 不声明的话 Windows 会把整个窗口按 DPI 比例做位图拉伸, 菜单文字与图标一起发虚。
/// 它还会影响 `SM_CXSMICON` 的返回值 —— 125% 缩放下返回 20 而不是 16,
/// 托盘图标因此才能按真实像素去挑选合适的尺寸。
///
/// PER_MONITOR_AWARE_V2 需要 Windows 10 1703+, 旧系统上调用失败可以直接忽略。
fn enable_dpi_awareness() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn init_logger(args: &Args) {
    let level = match args.verbose {
        0 => LevelFilter::Info,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    let mut builder = env_logger::Builder::new();
    builder.filter_level(level).format_timestamp(None);
    if args.logfile {
        match log_file_path().and_then(|p| std::fs::File::create(p).ok()) {
            Some(f) => {
                builder.target(env_logger::Target::Pipe(Box::new(f)));
            }
            None => eprintln!("无法创建日志文件, 继续输出到控制台"),
        }
    }
    builder.init();
}

fn log_file_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("keyremap.log")))
}

/// 主消息循环。
///
/// 这一个循环同时承担三件事, 这也是不需要额外线程的原因:
///   1. 派发低级钩子的回调 (系统在本线程的消息处理中调用它们)
///   2. 派发托盘窗口的消息, 右键菜单与菜单命令都在窗口过程里直接处理完
///   3. 接收文件监听线程 PostThreadMessage 过来的重载通知
fn run_app_loop() {
    let mut msg: MSG = unsafe { std::mem::zeroed() };

    loop {
        // 返回 0 表示 WM_QUIT, -1 表示出错
        let ret = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if ret <= 0 {
            break;
        }

        // 文件监听线程发来的重载完成通知。这类消息 hwnd 为空,
        // 不会被 DispatchMessage 派发给任何窗口, 只能在这里自己认。
        if msg.message == watcher::WM_CONFIG_RELOADED {
            tray::update_status();
            tray::notify("配置已重载", &tray_reload_text());
            continue;
        }

        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn tray_reload_text() -> String {
    let n = hook::config_snapshot()
        .map(|c| c.active_count())
        .unwrap_or(0);
    format!("{n} 条映射生效")
}

fn default_config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("keyremap.toml")))
        .unwrap_or_else(|| PathBuf::from("keyremap.toml"))
}

/// 键名表 JSON, 供 Web 配置工具作为唯一事实来源。
/// 手写 JSON 以免为一个调试命令引入 serde_json。
fn keys_json() -> String {
    let keys: Vec<&str> = keycode::KEY_TABLE.iter().map(|(n, _)| *n).collect();
    format!(
        "{{\n  \"keys\": [{}],\n  \"mouse\": [{}]\n}}",
        quote_join(&keys),
        quote_join(&[
            "MouseLeft",
            "MouseRight",
            "MouseMiddle",
            "MouseX1",
            "MouseX2"
        ])
    )
}

fn quote_join(items: &[&str]) -> String {
    items
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ")
}
