//! 开机自启动。
//!
//! 有两种机制, 用哪个取决于"是否以管理员身份运行"这个偏好:
//!
//! - **普通权限**: `HKCU\...\Run` 项。简单, 不需要任何特权。
//! - **管理员**:   任务计划程序里一个 `/RL HIGHEST` 的登录触发任务。
//!
//! 为什么管理员模式必须换机制: `HKCU\Run` 启动的进程**永远不会提权**,
//! 这是 Windows 的设计。想要"开机自启 + 管理员"只有计划任务这一条路。
//!
//! 两种机制互斥, 切换时必须清掉另一个 —— 否则会有两条自启路径同时生效。
//! (虽然单实例锁会挡掉第二个进程, 但那是靠巧合而不是设计。)

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use log::debug;
use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;

use crate::{elevate, paths, regutil, singleton};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "keyremap";

/// 不让 schtasks 闪出一个控制台窗口 —— 本程序是 GUI 子系统的。
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 计划任务名。带上 exe 路径的哈希, 这样复制到别处的另一份副本
/// 可以有自己的自启任务, 和单实例锁的粒度保持一致。
fn task_name() -> String {
    singleton::name_for_current_exe()
}

/// 当前是否已设置开机自启 (两种机制任一生效即算)。
pub fn is_enabled() -> bool {
    run_entry_is_self() || task_exists()
}

/// 按当前的管理员偏好启用自启。
///
/// 会顺手清掉另一种机制, 所以也可以当"切换机制"用。
pub fn enable(config_path: &Path) -> Result<(), String> {
    if elevate::wants_admin() {
        // 鸡生蛋: 创建 /RL HIGHEST 的任务本身就需要管理员权限
        if !elevate::is_elevated() {
            return Err("创建管理员级别的自启任务需要先以管理员身份运行".into());
        }
        remove_run_entry()?;
        create_task(config_path)
    } else {
        delete_task_quiet();
        write_run_entry(config_path)
    }
}

/// 关闭自启, 两种机制都清掉。
pub fn disable() -> Result<(), String> {
    let r = remove_run_entry();
    delete_task_quiet();
    r
}

/// 让实际使用的机制与当前偏好一致。启动时调用一次, 幂等。
///
/// 用途: 用户刚打开"以管理员运行"时还没有权限建任务, 提权重启之后
/// 由这里补上, 并把旧的 Run 项清掉。
pub fn sync(config_path: &Path) -> Result<(), String> {
    if !is_enabled() {
        return Ok(());
    }
    let admin_mode = elevate::wants_admin();
    let via_task = task_exists();

    // 已经在正确的机制上, 什么都不用做
    if admin_mode == via_task {
        return Ok(());
    }
    debug!(
        "自启机制需要切换: 当前={}, 期望={}",
        if via_task { "计划任务" } else { "Run 项" },
        if admin_mode {
            "计划任务"
        } else {
            "Run 项"
        }
    );
    enable(config_path)
}

// ---------- HKCU\Run ----------

/// Run 项存在且指向**当前这个 exe**。
///
/// 不只看键是否存在: 用户可能有多份副本, 注册表里那条若指向另一份,
/// 对本实例来说就不算"已启用"。
fn run_entry_is_self() -> bool {
    let Some(existing) = regutil::read_string(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE) else {
        return false;
    };
    // 必须和 launch_command 用同一个路径口径, 否则自己写下的项自己认不出来
    let prefix = format!("\"{}\"", paths::stable_exe().display()).to_lowercase();
    existing.to_lowercase().starts_with(&prefix)
}

fn write_run_entry(config_path: &Path) -> Result<(), String> {
    let cmd = launch_command(config_path)?;
    regutil::write_string(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, &cmd)
}

fn remove_run_entry() -> Result<(), String> {
    regutil::delete_value(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE)
}

// ---------- 任务计划程序 ----------

fn task_exists() -> bool {
    schtasks(&["/Query", "/TN", &task_name()]).is_ok()
}

fn create_task(config_path: &Path) -> Result<(), String> {
    let cmd = launch_command(config_path)?;
    schtasks(&[
        "/Create",
        "/F", // 已存在则覆盖
        "/TN",
        &task_name(),
        "/TR",
        &cmd,
        "/SC",
        "ONLOGON",
        "/RL",
        "HIGHEST", // 关键: 以最高可用权限运行, 开机自启时不弹 UAC
    ])
    .map(|_| ())
    .map_err(|e| format!("创建自启任务失败: {e}"))
}

/// 删除任务, 失败只记日志。任务本来不存在也会"失败", 那不算问题。
fn delete_task_quiet() {
    if let Err(e) = schtasks(&["/Delete", "/F", "/TN", &task_name()]) {
        debug!("删除自启任务未成功 (可能本来就没有): {e}");
    }
}

fn schtasks(args: &[&str]) -> Result<String, String> {
    let out = Command::new("schtasks")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("无法调用 schtasks: {e}"))?;

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        // schtasks 的中文输出是 OEM 编码, from_utf8_lossy 可能出乱码,
        // 但错误信息只进日志, 够用
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if msg.is_empty() {
            format!("schtasks 退出码 {:?}", out.status.code())
        } else {
            msg
        })
    }
}

// ---------- 公共 ----------

/// 自启时使用的命令行。
///
/// 两个要点:
///
/// - exe 路径走 [`paths::stable_exe`]。这条路径要在注册表或计划任务里存到下次开机,
///   而 scoop 的版本目录到那时可能已经不存在了。
/// - 配置正好是默认查找结果时不带 `-c`, 命令行更短也更不容易出错
///   (schtasks 的 /TR 对嵌套引号比较敏感)。省掉 `-c` 之后启动的实例会自己
///   重跑一遍查找, 结果和现在一致。
fn launch_command(config_path: &Path) -> Result<String, String> {
    let exe = paths::stable_exe();
    if exe.as_os_str().is_empty() {
        return Err("无法确定程序路径".into());
    }

    Ok(if paths::default_config().as_path() == config_path {
        format!("\"{}\"", exe.display())
    } else {
        format!("\"{}\" -c \"{}\"", exe.display(), config_path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn 默认配置路径不带参数() {
        let cmd = launch_command(&paths::default_config()).unwrap();
        assert!(!cmd.contains("-c"), "默认路径不该带 -c: {cmd}");
        assert!(cmd.starts_with('"') && cmd.ends_with('"'));
    }

    #[test]
    fn 非默认配置路径带上参数() {
        let other = PathBuf::from(r"D:\somewhere\my.toml");
        let cmd = launch_command(&other).unwrap();
        assert!(cmd.contains("-c"), "非默认路径应带 -c: {cmd}");
        assert!(cmd.contains("my.toml"));
        // 路径两侧都要有引号, 否则含空格的路径会被拆开
        assert!(cmd.contains(r#""D:\somewhere\my.toml""#));
    }

    #[test]
    fn 任务名与单实例锁同源() {
        // 两者都按 exe 路径区分, 保证多副本行为一致
        assert_eq!(task_name(), singleton::name_for_current_exe());
        assert!(task_name().starts_with("keyremap-"));
    }
}
