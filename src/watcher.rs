//! 配置文件监听与热重载。
//!
//! 两个容易踩的坑, 这里都绕开了:
//!
//! 1. **监听目录而不是文件**。多数编辑器保存时走的是"写临时文件 + 重命名覆盖"的路子,
//!    原文件的 inode/句柄会被替换, 直接盯着文件看会在第一次保存后就再也收不到事件。
//!
//! 2. **一次保存会产生多个事件** (内容写入、属性变更、重命名), 所以要防抖,
//!    否则一次 Ctrl+S 会触发好几轮重载。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use log::{debug, error, info, warn};
use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

use crate::{config, hook};

/// 配置重载完成后发给主线程的消息, 让它刷新托盘菜单。
pub const WM_CONFIG_RELOADED: u32 = WM_APP + 1;

/// 启动监听。返回的 Debouncer 必须被持有 —— 一旦 drop, 监听就停止了。
pub fn spawn(
    config_path: &Path,
    main_thread: u32,
) -> Result<Debouncer<RecommendedWatcher>, String> {
    let dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or("无法确定配置文件所在目录")?;

    let target: PathBuf = config_path.to_path_buf();

    // 记录上次处理过的内容哈希。一次保存往往会产生不止一轮文件系统事件
    // (内容写入与元数据更新可能间隔超过防抖窗口), 只靠时间防抖挡不住,
    // 结果就是同一份配置被重复加载、托盘菜单被重复重建。
    let last_hash = Mutex::new(
        std::fs::read_to_string(config_path)
            .map(|c| hash_of(&c))
            .unwrap_or(0),
    );

    let mut debouncer = new_debouncer(
        Duration::from_millis(400),
        move |res: DebounceEventResult| match res {
            Ok(events) => {
                if events.iter().any(|e| is_target(&e.path, &target)) {
                    reload_if_changed(&target, main_thread, &last_hash);
                }
            }
            Err(e) => warn!("文件监听出错: {e:?}"),
        },
    )
    .map_err(|e| format!("创建文件监听失败: {e}"))?;

    debouncer
        .watcher()
        .watch(&dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("监听目录 {} 失败: {e}", dir.display()))?;

    info!("已监听配置目录: {}", dir.display());
    Ok(debouncer)
}

/// 判断事件路径是否指向我们关心的配置文件。
/// 只比文件名: 事件里的路径可能是短路径或大小写不同的形式。
fn is_target(changed: &Path, target: &Path) -> bool {
    match (changed.file_name(), target.file_name()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// 立即重新读取配置并替换到钩子层, 返回生效的映射条数。
/// 托盘菜单的"重新加载配置"走这条路径。
///
/// **只能在跑消息循环的线程上调用** (托盘的窗口过程就在那个线程)。
/// 里面会按新配置增减鼠标钩子, 而低级钩子只在安装它的线程上被回调。
/// 文件监听线程不能直接调这里, 它走的是"改配置 + 发消息让主线程收尾"。
pub fn reload_now(path: &Path) -> Result<usize, String> {
    let cfg = config::load(path)?;
    let n = cfg.active_count();
    config::warn_conflicts(&cfg);
    // 先换配置 (顺带换掉窗口规则表), 再让两个"按需安装"的钩子跟上
    hook::set_config(cfg);
    hook::sync_mouse_hook()?;
    crate::foreground::sync()?;
    Ok(n)
}

/// 文件变化触发的重载。内容与上次相同则直接跳过。
///
/// 解析失败时**保留原有配置继续运行** —— 编辑器保存到一半的文件常常不是合法 TOML,
/// 因为一次语法错误就让重映射整个失效是不可接受的。
fn reload_if_changed(path: &Path, main_thread: u32, last_hash: &Mutex<u64>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!("读取配置失败: {e}");
            return;
        }
    };

    let hash = hash_of(&content);
    match last_hash.lock() {
        Ok(mut guard) => {
            if *guard == hash {
                debug!("配置内容未变化, 跳过重载");
                return;
            }
            // 无论后面解析成功与否都记下这份内容: 解析失败时也不必对同一份
            // 坏配置反复报错, 等用户改动后哈希自然会变。
            *guard = hash;
        }
        Err(_) => return,
    }

    match config::parse(&content) {
        Ok(cfg) => {
            let n = cfg.active_count();
            config::warn_conflicts(&cfg);
            hook::set_config(cfg);
            info!("配置已重载, {n} 条映射生效");
            // 通知主线程刷新托盘菜单。用 PostThreadMessage 而不是共享标志位,
            // 是因为主线程阻塞在 GetMessageW 上, 需要一条消息把它唤醒。
            unsafe {
                PostThreadMessageW(main_thread, WM_CONFIG_RELOADED, 0, 0);
            }
        }
        Err(e) => error!("配置重载失败, 继续沿用上一份配置: {e}"),
    }
}

fn hash_of(content: &str) -> u64 {
    let mut h = DefaultHasher::new();
    content.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 按文件名匹配且忽略大小写() {
        let target = Path::new(r"D:\app\keyremap.toml");
        assert!(is_target(Path::new(r"D:\app\KEYREMAP.TOML"), target));
        assert!(is_target(Path::new(r"C:\other\keyremap.toml"), target));
        assert!(!is_target(Path::new(r"D:\app\other.toml"), target));
    }
}
