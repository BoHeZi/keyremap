//! 单实例保护。
//!
//! 键盘映射必须保证只有一个实例: 多个进程各装一套低级钩子后, 事件会被逐层处理,
//! 表现出"禁用了却还在生效""一次按键触发两回注入"这类难以排查的怪象。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// 生成与**当前 exe 路径绑定**的互斥体名。
///
/// 这样限制的是"同一个副本只能跑一个实例", 而不是"整台机器只能跑一个"。
/// 用户把程序复制到另一个目录、想用不同配置同时跑两份, 是合理的用法,
/// 全局单例会把这种用法一并禁掉。
///
/// Windows 路径不区分大小写, 所以统一转小写再哈希, 免得同一个文件
/// 因为大小写写法不同被当成两份。
///
/// 用 [`crate::paths::stable_exe`] 而不是 `current_exe`: scoop 那种带版本号的
/// 安装目录会让锁名每次升级都变, 于是"是否已启用自启"的判断跟着失灵。
pub fn name_for_current_exe() -> String {
    let path = crate::paths::stable_exe();
    let mut h = DefaultHasher::new();
    path.to_string_lossy().to_lowercase().hash(&mut h);
    format!("keyremap-{:016x}", h.finish())
}

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
