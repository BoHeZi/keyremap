//! 开机自启动, 通过 HKCU 的 Run 注册表项实现。
//!
//! 用 HKCU 而不是 HKLM: 前者不需要管理员权限, 且只影响当前用户 —— 键盘映射
//! 本来就是个人偏好。代价是这样启动的进程**不会提权**, 想要"开机自启且以管理员运行"
//! 得走任务计划程序, 见 [`elevate`](crate::elevate) 里的说明。

use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "keyremap-ng";

/// 当前是否已设置为开机自启。
///
/// 不只看键存不存在, 还要确认它指向的是**当前这个 exe**: 用户可能有多份副本,
/// 注册表里那条如果指向另一份, 对本实例来说就不算"已启用"。
pub fn is_enabled() -> bool {
    let Some(current) = command_line() else {
        return false;
    };
    match read_value() {
        Some(existing) => paths_equal(&existing, &current),
        None => false,
    }
}

/// 开启自启动。写入的命令包含配置文件路径, 保证开机后加载的是同一份配置。
pub fn enable(config_path: &Path) -> Result<(), String> {
    let cmd = command_line_with(config_path).ok_or("无法确定程序路径")?;
    write_value(&cmd)
}

/// 关闭自启动。
pub fn disable() -> Result<(), String> {
    delete_value()
}

/// 当前 exe 的启动命令 (不带配置参数), 用于比对。
fn command_line() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

fn command_line_with(config_path: &Path) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!(
        "\"{}\" -c \"{}\"",
        exe.display(),
        config_path.display()
    ))
}

/// 比较注册表里的命令行是否指向当前 exe。
/// 注册表里的值带着 `-c` 参数, 所以只比较开头的可执行文件部分, 且忽略大小写。
fn paths_equal(registered: &str, current_prefix: &str) -> bool {
    registered
        .to_lowercase()
        .starts_with(&current_prefix.to_lowercase())
}

// ---------- 注册表读写 ----------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_run_key(access: u32) -> Option<HKEY> {
    let sub = wide(RUN_KEY);
    let mut key: HKEY = ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, access, &mut key) };
    if rc == ERROR_SUCCESS { Some(key) } else { None }
}

fn read_value() -> Option<String> {
    let key = open_run_key(KEY_QUERY_VALUE)?;
    let name = wide(VALUE_NAME);
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;

    let rc = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };

    if rc != ERROR_SUCCESS {
        return None;
    }
    // size 是字节数, 且包含结尾的 NUL
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len.min(buf.len())]))
}

fn write_value(cmd: &str) -> Result<(), String> {
    let key = open_run_key(KEY_SET_VALUE).ok_or("无法打开注册表 Run 项")?;
    let name = wide(VALUE_NAME);
    let data = wide(cmd);

    let rc = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr() as *const u8,
            (data.len() * 2) as u32,
        )
    };
    unsafe { RegCloseKey(key) };

    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("写入注册表失败, 错误码 {rc}"))
    }
}

fn delete_value() -> Result<(), String> {
    let key = open_run_key(KEY_SET_VALUE).ok_or("无法打开注册表 Run 项")?;
    let name = wide(VALUE_NAME);
    let rc = unsafe { RegDeleteValueW(key, name.as_ptr()) };
    unsafe { RegCloseKey(key) };

    // 本来就没有也算成功
    if rc == ERROR_SUCCESS || rc == 2 {
        Ok(())
    } else {
        Err(format!("删除注册表项失败, 错误码 {rc}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 命令行前缀比对忽略大小写() {
        assert!(paths_equal(
            r#""D:\App\Keyremap-NG.exe" -c "D:\App\keyremap.toml""#,
            r#""d:\app\keyremap-ng.exe""#
        ));
    }

    #[test]
    fn 指向别处的自启项不算已启用() {
        assert!(!paths_equal(
            r#""C:\Other\keyremap-ng.exe" -c "x.toml""#,
            r#""d:\app\keyremap-ng.exe""#
        ));
    }
}
