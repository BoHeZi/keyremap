//! 注册表读写的薄封装。
//!
//! 抽出来是因为自启动 (字符串值) 与"以管理员运行"偏好 (DWORD 值) 都要访问注册表,
//! 各写一套 open/query/close 模板只会让两处都难改。

use std::ptr;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

/// 值不存在时的错误码, 当作"没有"而不是失败。
const ERROR_FILE_NOT_FOUND: u32 = 2;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 打开已存在的键。键不存在返回 None。
fn open(root: HKEY, sub: &str, access: u32) -> Option<HKEY> {
    let path = wide(sub);
    let mut key: HKEY = ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, access, &mut key) };
    if rc == ERROR_SUCCESS { Some(key) } else { None }
}

/// 打开或创建键, 用于写入。
fn open_or_create(root: HKEY, sub: &str) -> Option<HKEY> {
    let path = wide(sub);
    let mut key: HKEY = ptr::null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            root,
            path.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            ptr::null(),
            &mut key,
            ptr::null_mut(),
        )
    };
    if rc == ERROR_SUCCESS { Some(key) } else { None }
}

pub fn read_dword(root: HKEY, sub: &str, name: &str) -> Option<u32> {
    let key = open(root, sub, KEY_QUERY_VALUE)?;
    let name_w = wide(name);
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;

    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            &mut value as *mut u32 as *mut u8,
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };

    if rc == ERROR_SUCCESS {
        Some(value)
    } else {
        None
    }
}

pub fn write_dword(root: HKEY, sub: &str, name: &str, value: u32) -> Result<(), String> {
    let key = open_or_create(root, sub).ok_or_else(|| format!("无法打开注册表键 {sub}"))?;
    let name_w = wide(name);
    let rc = unsafe {
        RegSetValueExW(
            key,
            name_w.as_ptr(),
            0,
            REG_DWORD,
            &value as *const u32 as *const u8,
            size_of::<u32>() as u32,
        )
    };
    unsafe { RegCloseKey(key) };

    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("写入 {sub}\\{name} 失败, 错误码 {rc}"))
    }
}

/// 读字符串值。
///
/// 先问长度再按需分配, 不用固定缓冲区: 值超过缓冲区时 `RegQueryValueExW`
/// 返回 `ERROR_MORE_DATA` 而不是截断, 把那当成"值不存在"会让
/// `autostart::run_entry_is_self` 误判成未启用自启。
pub fn read_string(root: HKEY, sub: &str, name: &str) -> Option<String> {
    let key = open(root, sub, KEY_QUERY_VALUE)?;
    let name_w = wide(name);

    // 数据指针传 null 时只回填所需字节数
    let mut size = 0u32;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        )
    };
    if rc != ERROR_SUCCESS || size == 0 {
        unsafe { RegCloseKey(key) };
        return None;
    }

    // size 是字节数, 奇数字节向上取整成完整的 u16
    let mut buf = vec![0u16; size.div_ceil(2) as usize];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name_w.as_ptr(),
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
    // 注册表**不保证** REG_SZ 以 NUL 结尾, 所以按实际出现的 NUL 截断
    // 而不是无条件把最后一个字符当结束符减掉 —— 后者会吃掉真实字符。
    let len = (size as usize / 2).min(buf.len());
    let data = &buf[..len];
    let end = data.iter().position(|&c| c == 0).unwrap_or(len);
    Some(String::from_utf16_lossy(&data[..end]))
}

pub fn write_string(root: HKEY, sub: &str, name: &str, value: &str) -> Result<(), String> {
    let key = open_or_create(root, sub).ok_or_else(|| format!("无法打开注册表键 {sub}"))?;
    let name_w = wide(name);
    let data = wide(value);
    let rc = unsafe {
        RegSetValueExW(
            key,
            name_w.as_ptr(),
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
        Err(format!("写入 {sub}\\{name} 失败, 错误码 {rc}"))
    }
}

/// 删除值。本来就不存在也算成功 —— 调用方要的是"最终没有这个值"。
pub fn delete_value(root: HKEY, sub: &str, name: &str) -> Result<(), String> {
    let Some(key) = open(root, sub, KEY_SET_VALUE) else {
        return Ok(()); // 键都没有, 值自然也没有
    };
    let name_w = wide(name);
    let rc = unsafe { RegDeleteValueW(key, name_w.as_ptr()) };
    unsafe { RegCloseKey(key) };

    if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("删除 {sub}\\{name} 失败, 错误码 {rc}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;

    /// 测试专用的键, 绝不能用真实的那些 —— 否则跑一次测试就把用户的
    /// 自启设置或管理员偏好改掉了。
    const TEST_KEY: &str = r"Software\keyremap-selftest";

    #[test]
    fn dword_读写与删除() {
        write_dword(HKEY_CURRENT_USER, TEST_KEY, "num", 42).expect("写入应当成功");
        assert_eq!(read_dword(HKEY_CURRENT_USER, TEST_KEY, "num"), Some(42));

        // 覆盖写
        write_dword(HKEY_CURRENT_USER, TEST_KEY, "num", 0).unwrap();
        assert_eq!(read_dword(HKEY_CURRENT_USER, TEST_KEY, "num"), Some(0));

        delete_value(HKEY_CURRENT_USER, TEST_KEY, "num").unwrap();
        assert_eq!(read_dword(HKEY_CURRENT_USER, TEST_KEY, "num"), None);
    }

    #[test]
    fn 字符串读写与删除() {
        // 带引号和中文, 这两样在实际的自启命令行里都会出现
        let value = r#""D:\某个 目录\keyremap.exe" -c "D:\某个 目录\keyremap.toml""#;
        write_string(HKEY_CURRENT_USER, TEST_KEY, "cmd", value).expect("写入应当成功");
        assert_eq!(
            read_string(HKEY_CURRENT_USER, TEST_KEY, "cmd").as_deref(),
            Some(value)
        );

        delete_value(HKEY_CURRENT_USER, TEST_KEY, "cmd").unwrap();
        assert_eq!(read_string(HKEY_CURRENT_USER, TEST_KEY, "cmd"), None);
    }

    /// 固定 1024 个 u16 的旧实现会在这里拿到 ERROR_MORE_DATA 并返回 None,
    /// 于是"自启已启用"被误判成未启用。
    #[test]
    fn 超长字符串不会被当成不存在() {
        let value = "x".repeat(4096);
        write_string(HKEY_CURRENT_USER, TEST_KEY, "long", &value).expect("写入应当成功");
        assert_eq!(
            read_string(HKEY_CURRENT_USER, TEST_KEY, "long").as_deref(),
            Some(value.as_str())
        );
        delete_value(HKEY_CURRENT_USER, TEST_KEY, "long").unwrap();
    }

    #[test]
    fn 读不存在的值返回none() {
        assert_eq!(
            read_dword(HKEY_CURRENT_USER, r"Software\keyremap-nonexistent", "x"),
            None
        );
        assert_eq!(
            read_string(HKEY_CURRENT_USER, r"Software\keyremap-nonexistent", "x"),
            None
        );
    }

    #[test]
    fn 删除不存在的值也算成功() {
        // 调用方要的是"最终没有这个值", 本来就没有不该算失败
        assert!(delete_value(HKEY_CURRENT_USER, TEST_KEY, "never-written").is_ok());
        assert!(delete_value(HKEY_CURRENT_USER, r"Software\keyremap-nonexistent", "x").is_ok());
    }
}
