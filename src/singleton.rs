//! 单实例保护。
//!
//! 键盘映射必须保证只有一个实例: 多个进程各装一套低级钩子后, 事件会被逐层处理,
//! 表现出"禁用了却还在生效""一次按键触发两回注入"这类难以排查的怪象。

use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// 持有一个命名互斥体, 存活期间其他实例无法启动。
/// drop 时释放, 所以只要把它保留在 main 的作用域里即可。
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// 尝试取得单实例锁。已有实例在运行时返回 `None`。
    ///
    /// 用 `Local\` 而非 `Global\` 前缀: 后者在受限环境下需要
    /// SeCreateGlobalPrivilege 权限才能创建, 而"每个登录会话一个实例"
    /// 本来就是这个工具需要的语义。
    pub fn acquire(name: &str) -> Option<Self> {
        let wide: Vec<u16> = format!("Local\\{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateMutexW(ptr::null(), 1, wide.as_ptr());
            if handle.is_null() {
                return None;
            }
            // 注意顺序: CreateMutexW 成功时也可能是"已存在", 要靠 GetLastError 区分
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
