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

use std::path::PathBuf;

use clap::Parser;
use log::{LevelFilter, error, info};

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

    let config_path = args.config.unwrap_or_else(default_config_path);
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

    info!("=== 运行中, Ctrl+C 退出 ===");
    hook::run_message_loop();
    hook::uninstall();
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
