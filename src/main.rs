//! keyremap-ng: Windows 键盘/鼠标重映射。
//!
//! 里程碑 1: 自实现低级钩子层, 验证核心可行性。
//! 相比基于 rdev 的旧版, 这一层解决了三个实际问题:
//!   1. 不再调用 ToUnicodeEx, 不会干扰输入法与死键状态
//!   2. 注入事件带 dwExtraInfo 标记, A<->B 互换映射不再回环
//!   3. 组合键用单次原子 SendInput, 回调内无 sleep, 不触发钩子超时被摘

mod config;
mod hook;
mod inject;
mod keycode;
mod tray;
mod watcher;

use std::path::{Path, PathBuf};
use std::ptr;

use clap::Parser;
use log::{LevelFilter, error, info, warn};
use tray_icon::menu::MenuEvent;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostQuitMessage, TranslateMessage,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "Windows 键盘/鼠标重映射工具", long_about = None)]
struct Args {
    /// 配置文件路径, 默认为可执行文件同目录下的 keyremap.toml
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// 提高日志级别, 可重复: -v, -vv
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
}

fn main() {
    let args = Args::parse();

    let level = match args.verbose {
        0 => LevelFilter::Info,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    env_logger::Builder::new()
        .filter_level(level)
        .format_timestamp(None)
        .init();

    if args.dump_keys {
        print_keys_json();
        return;
    }

    // 监听模式不需要配置文件
    if args.listen {
        info!("=== 监听模式 (不做映射), Ctrl+C 退出 ===");
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
            std::process::exit(1);
        }
    };

    if args.dump {
        println!("配置名称: {}", cfg.name);
        for m in &cfg.mappings {
            println!(
                "  [{}] {:<20} {}",
                if m.enable { "on " } else { "off" },
                m.name,
                m
            );
            if !m.comment.is_empty() {
                println!("        {}", m.comment);
            }
        }
        return;
    }

    let with_mouse = cfg.needs_mouse_hook();
    info!(
        "已启用 {} 条映射{}",
        cfg.enabled_count(),
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

    let mut tray = match tray::Tray::new(&config_path) {
        Ok(t) => t,
        Err(e) => {
            error!("{e}");
            hook::uninstall();
            std::process::exit(1);
        }
    };

    info!("=== 运行中, 通过托盘菜单退出 ===");
    run_app_loop(&mut tray, &config_path);
    hook::uninstall();
}

/// 主消息循环。
///
/// 这一个循环同时承担三件事, 这也是不需要额外线程的原因:
///   1. 派发低级钩子的回调 (系统在本线程的消息处理中调用它们)
///   2. 派发托盘窗口的消息, 菜单点击由此产生
///   3. 接收文件监听线程 PostThreadMessage 过来的重载通知
fn run_app_loop(tray: &mut tray::Tray, config_path: &Path) {
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
            tray.refresh();
            continue;
        }

        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 菜单点击是在上面 DispatchMessage 处理托盘窗口消息时投递到 channel 的,
        // 所以紧接着取一次就能拿到。
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match tray.on_menu(&event.id) {
                tray::Action::Quit => {
                    info!("退出");
                    unsafe { PostQuitMessage(0) };
                }
                tray::Action::Reload => match watcher::reload_now(config_path) {
                    Ok(n) => {
                        info!("已手动重载配置, {n} 条映射生效");
                        tray.refresh();
                    }
                    Err(e) => error!("{e}"),
                },
                tray::Action::Nothing => {}
            }
        }
    }
}

fn default_config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("keyremap.toml")))
        .unwrap_or_else(|| PathBuf::from("keyremap.toml"))
}

/// 输出键名表, 供 Web 配置工具作为唯一事实来源。
/// 手写 JSON 以免为一个调试命令引入 serde_json。
fn print_keys_json() {
    let keys: Vec<&str> = keycode::KEY_TABLE.iter().map(|(n, _)| *n).collect();
    println!("{{");
    println!("  \"keys\": [{}],", quote_join(&keys));
    println!(
        "  \"mouse\": [{}]",
        quote_join(&[
            "MouseLeft",
            "MouseRight",
            "MouseMiddle",
            "MouseX1",
            "MouseX2"
        ])
    );
    println!("}}");
}

fn quote_join(items: &[&str]) -> String {
    items
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ")
}
