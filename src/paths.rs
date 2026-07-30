//! 配置文件与可执行文件的位置。
//!
//! 单独成一个模块, 是因为"东西放哪"这件事被两种分发方式拉向相反的方向:
//!
//! - **绿色版**: 解压即用, 配置就在 exe 旁边, 整个目录拷走就带走了设置。
//! - **安装版** (scoop 之类): 程序装在带版本号的目录里, 升级等于换一个新目录,
//!   旧目录连同里面的一切作废。
//!
//! 两边都要照顾, 于是有了下面的 [`default_config`] 查找顺序和 [`stable_exe`]。

use std::path::{Path, PathBuf};

use log::info;

/// 首次运行时落地的配置模板。编进二进制而不是靠外部文件,
/// 这样光有一个 exe 也能把配置建起来。
const DEFAULT_CONFIG: &str = include_str!("../assets/default.toml");

const CONFIG_NAME: &str = "keyremap.toml";
/// 安装版的配置目录名 (位于 %APPDATA% 下)。
const APP_DIR: &str = "keyremap";

/// scoop 用来指向当前版本的联接名。
const SCOOP_CURRENT: &str = "current";
/// scoop 存放所有应用的目录名。
const SCOOP_APPS: &str = "apps";

// ---------- scoop 布局识别 ----------

/// 纯路径运算: exe 形如 `...\apps\<名字>\<某一层>\x.exe` 时,
/// 返回 `...\apps\<名字>\current`。不看文件系统, 所以能直接测。
///
/// 要求 `apps` 正好是 exe 所在目录的祖父, 层级不对就不算。
fn scoop_current_dir(exe: &Path) -> Option<PathBuf> {
    let app_dir = exe.parent()?.parent()?;
    if !app_dir
        .parent()?
        .file_name()?
        .eq_ignore_ascii_case(SCOOP_APPS)
    {
        return None;
    }
    Some(app_dir.join(SCOOP_CURRENT))
}

/// 在上面的基础上再确认 `current` 目录真的存在 —— 这是 scoop 的标志。
///
/// 两个条件都要满足才算, 只看目录名会误伤那些自己恰好装在
/// `X\apps\某名字\某目录\` 下的普通绿色版。
fn scoop_current_dir_checked(exe: &Path) -> Option<PathBuf> {
    scoop_current_dir(exe).filter(|p| p.is_dir())
}

// ---------- 可执行文件 ----------

/// 可执行文件路径, 但**跨版本稳定**。
///
/// scoop 把程序装成这样:
///
/// ```text
/// scoop\apps\keyremap\0.1.0\keyremap.exe   <- 真实文件
/// scoop\apps\keyremap\current              -> 联接, 指向当前版本目录
/// ```
///
/// 凡是要**写下来留到以后用**的路径 (开机自启的命令行、计划任务的 /TR) 都必须
/// 走 `current`。记下带版本号的那条, `scoop update` 换掉版本目录之后自启就
/// 指向一个已经不存在的目录 —— 而且是静默失效, 用户只会发现"升级完就不自启了"。
///
/// 单实例名同样按 exe 路径哈希, 用带版本号的路径会让"是否已启用自启"的判断
/// 在每次升级后失灵, 于是托盘里的勾莫名消失、旧的计划任务变成孤儿。
///
/// 通过 shim 启动时拿到的本来就是 `current` 那条, 这里是无操作。
pub fn stable_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    match (scoop_current_dir_checked(&exe), exe.file_name()) {
        (Some(current), Some(file)) => current.join(file),
        _ => exe,
    }
}

// ---------- 配置文件 ----------

/// 默认配置文件的位置。
///
/// 查找顺序:
///
/// 1. **exe 同目录**的 `keyremap.toml` —— 存在就用它。绿色版靠这条,
///    行为和以前完全一样: 拷到 U 盘的那份始终读自己带的配置。
/// 2. **`%APPDATA%\keyremap\keyremap.toml`** —— 安装版落在这里,
///    不会随程序目录被升级换掉。
///
/// 顺序不能颠倒。反过来的话, 绿色版用户明明看见 exe 旁边有个配置文件,
/// 程序却在读另一个地方, 这没法解释。
///
/// **例外**: exe 在 scoop 的应用目录里时, 第 1 条整个跳过。发布包里自带的
/// `keyremap.toml` 会被解压进版本目录, 若当成绿色版配置用, 用户改了它,
/// 下次 `scoop update` 换目录就一起没了 —— 静默丢配置。
///
/// 两处都没有时返回 %APPDATA% 那条, 由 [`ensure_config`] 建出来。选它而不选
/// exe 同目录, 是因为程序目录未必可写 (Program Files) 也未必长久 (scoop 版本目录),
/// %APPDATA% 两样都满足。
pub fn default_config() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();

    if let Some(beside) = exe.parent().map(|d| d.join(CONFIG_NAME))
        && beside.is_file()
    {
        if scoop_current_dir_checked(&exe).is_none() {
            return beside;
        }
        // 明说一句。否则用户对着安装目录里那份改半天没反应, 无从下手。
        info!(
            "忽略 {} (scoop 安装目录会随升级失效, 那份只作示例), 改用用户目录下的配置",
            beside.display()
        );
    }

    roaming_config().unwrap_or_else(|| PathBuf::from(CONFIG_NAME))
}

/// `%APPDATA%\keyremap\keyremap.toml`。
///
/// 用 APPDATA (漫游) 而不是 LOCALAPPDATA: 按键映射是纯粹的用户偏好,
/// 在配了漫游的环境里跟着账号走正是想要的效果。
fn roaming_config() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    if base.is_empty() {
        return None;
    }
    Some(Path::new(&base).join(APP_DIR).join(CONFIG_NAME))
}

/// 配置文件不存在时写一份模板出来。返回是否真的新建了。
///
/// 只对默认路径调用。`-c` 显式指定的路径不该自动创建 —— 用户把文件名敲错时,
/// 建一个空配置远不如直接报错有用。
pub fn ensure_config(path: &Path) -> Result<bool, String> {
    if path.is_file() {
        return Ok(false);
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("创建配置目录 {} 失败: {e}", dir.display()))?;
    }
    std::fs::write(path, DEFAULT_CONFIG)
        .map_err(|e| format!("写入默认配置 {} 失败: {e}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个临时目录, 名字带上用例名避免并行跑测试时互相干扰。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("keyremap-paths-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- 纯路径判定 ----

    #[test]
    fn 版本目录推出current目录() {
        assert_eq!(
            scoop_current_dir(Path::new(
                r"C:\Users\me\scoop\apps\keyremap\0.1.0\keyremap.exe"
            )),
            Some(PathBuf::from(r"C:\Users\me\scoop\apps\keyremap\current"))
        );
    }

    #[test]
    fn 从current启动时推出同一个目录() {
        // 通过 shim 启动就是这条路径, 结果必须和上面一致, 这样 stable_exe 是幂等的
        assert_eq!(
            scoop_current_dir(Path::new(
                r"C:\Users\me\scoop\apps\keyremap\current\keyremap.exe"
            )),
            Some(PathBuf::from(r"C:\Users\me\scoop\apps\keyremap\current"))
        );
    }

    #[test]
    fn 普通路径不算scoop布局() {
        // 绿色版解压到任意目录
        assert_eq!(
            scoop_current_dir(Path::new(r"D:\tools\keyremap\keyremap.exe")),
            None
        );
        // 层级不够
        assert_eq!(scoop_current_dir(Path::new(r"D:\keyremap.exe")), None);
        // apps 必须是 exe 所在目录的祖父; 这里它是父目录
        assert_eq!(
            scoop_current_dir(Path::new(r"D:\apps\keyremap\keyremap.exe")),
            None
        );
    }

    // ---- 加上文件系统检查 ----

    #[test]
    fn 没有current目录就不认() {
        // 用户偏偏把绿色版解压到 X\apps\keyremap\portable\ 这种位置。
        // 目录名对得上但没有 current, 不能当成 scoop。
        let root = temp_dir("no-current");
        let exe_dir = root.join(SCOOP_APPS).join("keyremap").join("portable");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let exe = exe_dir.join("keyremap.exe");

        assert!(scoop_current_dir(&exe).is_some(), "路径形状是对得上的");
        assert_eq!(
            scoop_current_dir_checked(&exe),
            None,
            "但 current 不存在, 不该认成 scoop"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn 有current目录才认() {
        let root = temp_dir("with-current");
        let app_dir = root.join(SCOOP_APPS).join("keyremap");
        std::fs::create_dir_all(app_dir.join("0.1.0")).unwrap();
        std::fs::create_dir_all(app_dir.join(SCOOP_CURRENT)).unwrap();
        let exe = app_dir.join("0.1.0").join("keyremap.exe");

        assert_eq!(
            scoop_current_dir_checked(&exe),
            Some(app_dir.join(SCOOP_CURRENT))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- 内置模板 ----

    #[test]
    fn 内置模板本身是合法配置() {
        // 模板要是写坏了, 首次运行直接弹错误框, 而那时用户手上还没有任何配置
        let cfg = crate::config::parse(DEFAULT_CONFIG).expect("内置模板必须能解析");
        assert_eq!(cfg.mappings.len(), 0, "模板不该预置任何映射");
        assert!(!cfg.name.is_empty());
    }

    // ---- 落地 ----

    #[test]
    fn 已存在的配置不会被覆盖() {
        let dir = temp_dir("keep");
        let path = dir.join(CONFIG_NAME);
        std::fs::write(&path, "name = \"用户的配置\"\n").unwrap();

        assert_eq!(ensure_config(&path), Ok(false), "已存在时不该报告新建");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "name = \"用户的配置\"\n",
            "内容必须原封不动"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 缺失时连目录一起建出来() {
        let dir = temp_dir("seed");
        // 故意多一层不存在的子目录, 验证 create_dir_all 生效
        let path = dir.join("sub").join(CONFIG_NAME);

        assert_eq!(ensure_config(&path), Ok(true));
        assert!(path.is_file());
        assert!(crate::config::load(&path).is_ok(), "生成的文件必须能加载");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
