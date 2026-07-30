//! 配置的解析与"编译"。
//!
//! 配置文件里写的是键名字符串, 但钩子回调是全系统按键的必经之路,
//! 绝不能在那里做字符串比较。所以加载时一次性把键名解析成虚拟键码,
//! 运行期只比较整数。键名写错也在这一步就报出来, 而不是等到按下才失败。

use std::fmt;
use std::path::Path;

use serde::Deserialize;

use crate::inject::MAX_COMBO;
use crate::keycode::{Input, input_from_name, name_from_mouse, name_from_vk, vk_from_name};

// ---------- 文件里的原始结构 ----------

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    name: String,
    #[serde(default)]
    mappings: Vec<RawMapping>,
}

#[derive(Debug, Deserialize)]
struct RawMapping {
    #[serde(default)]
    name: String,
    #[serde(default)]
    comment: String,
    #[serde(default = "default_true")]
    enable: bool,
    from: String,
    to: RawOutput,
}

/// `to` 既可以写成 `to = "Insert"`, 也可以写成 `to = ["Ctrl", "W"]`。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawOutput {
    Single(String),
    Combination(Vec<String>),
}

fn default_true() -> bool {
    true
}

// ---------- 编译后的运行期结构 ----------

/// 一条编译好的映射。热路径上只会读 `enable` / `from` / `to`。
#[derive(Debug, Clone)]
pub struct Mapping {
    pub name: String,
    pub comment: String,
    pub enable: bool,
    pub from: Input,
    /// 目标按键序列。长度为 1 表示单键映射, 大于 1 表示组合键。
    pub to: Vec<u16>,
}

impl Mapping {
    /// 是否是组合键映射。单键与组合键的触发时机不同, 见 `hook::handle`。
    pub fn is_combo(&self) -> bool {
        self.to.len() > 1
    }

    /// 菜单项与日志里显示的标题。没写 name 时退回映射本身的描述。
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            self.to_string()
        } else {
            format!("{}  ({})", self.name, self)
        }
    }
}

impl fmt::Display for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let from = match self.from {
            Input::Key(vk) => name_from_vk(vk).unwrap_or("?").to_string(),
            Input::Mouse(btn) => name_from_mouse(btn).to_string(),
        };
        let to = self
            .to
            .iter()
            .map(|vk| name_from_vk(*vk).unwrap_or("?"))
            .collect::<Vec<_>>()
            .join(" + ");
        write!(f, "{from} -> {to}")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub name: String,
    pub mappings: Vec<Mapping>,
}

impl Config {
    /// 配置里是否存在鼠标映射。
    ///
    /// 没有的话就不安装鼠标钩子 —— `WH_MOUSE_LL` 会收到全部鼠标移动事件,
    /// 鼠标动一下就进一次回调, 这是常态下最大的一笔无谓开销。
    ///
    /// 注意这里**不看 enable**: 托盘菜单可以随时把某条鼠标映射打开,
    /// 若按启用状态决定是否装钩子, 那条映射打开后会不生效。
    pub fn needs_mouse_hook(&self) -> bool {
        self.mappings
            .iter()
            .any(|m| matches!(m.from, Input::Mouse(_)))
    }

    pub fn enabled_count(&self) -> usize {
        self.mappings.iter().filter(|m| m.enable).count()
    }
}

/// 从文件加载并编译配置。
pub fn load(path: &Path) -> Result<Config, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("读取配置文件失败 ({}): {e}", path.display()))?;
    parse(&content)
}

/// 从字符串解析并编译配置。独立出来是为了能直接写单元测试。
pub fn parse(content: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(content).map_err(|e| format!("配置格式错误: {e}"))?;

    let mut mappings = Vec::with_capacity(raw.mappings.len());
    for (i, rm) in raw.mappings.into_iter().enumerate() {
        // 报错时带上条目序号和名字, 方便定位是哪一条写错了
        let label = if rm.name.is_empty() {
            format!("第 {} 条映射", i + 1)
        } else {
            format!("映射 \"{}\"", rm.name)
        };

        let from = input_from_name(&rm.from)
            .ok_or_else(|| format!("{label}: 未知的按键名 \"{}\"", rm.from))?;

        let to_names = match rm.to {
            RawOutput::Single(s) => vec![s],
            RawOutput::Combination(v) => v,
        };

        if to_names.is_empty() {
            return Err(format!("{label}: to 不能为空"));
        }
        if to_names.len() > MAX_COMBO {
            return Err(format!(
                "{label}: 组合键最多 {MAX_COMBO} 个, 实际 {}",
                to_names.len()
            ));
        }

        let mut to = Vec::with_capacity(to_names.len());
        for name in &to_names {
            let vk = vk_from_name(name).ok_or_else(|| {
                format!("{label}: 未知的按键名 \"{name}\" (输出目前只支持键盘按键)")
            })?;
            to.push(vk);
        }

        mappings.push(Mapping {
            name: rm.name,
            comment: rm.comment,
            enable: rm.enable,
            from,
            to,
        });
    }

    Ok(Config {
        name: raw.name,
        mappings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keycode::MouseButton;

    const SAMPLE: &str = r#"
name = "测试配置"

[[mappings]]
name = "Pause转Insert"
from = "Pause"
to = "Insert"

[[mappings]]
name = "侧键转Ctrl+W"
from = "MouseX2"
to = ["Ctrl", "W"]

[[mappings]]
name = "禁用项"
enable = false
from = "D"
to = "E"
"#;

    #[test]
    fn 解析单键与组合键() {
        let cfg = parse(SAMPLE).expect("应当解析成功");
        assert_eq!(cfg.mappings.len(), 3);

        assert!(!cfg.mappings[0].is_combo());
        assert!(cfg.mappings[1].is_combo());
        assert_eq!(cfg.mappings[1].to.len(), 2);
    }

    #[test]
    fn 鼠标输入被正确识别() {
        let cfg = parse(SAMPLE).unwrap();
        assert_eq!(cfg.mappings[1].from, Input::Mouse(MouseButton::X2));
        assert!(cfg.needs_mouse_hook());
    }

    #[test]
    fn 禁用项不计入启用数() {
        let cfg = parse(SAMPLE).unwrap();
        assert_eq!(cfg.enabled_count(), 2);
        assert!(!cfg.mappings[2].enable);
    }

    #[test]
    fn 未知键名报错且指明位置() {
        let err = parse(
            r#"
[[mappings]]
name = "错误项"
from = "NoSuchKey"
to = "A"
"#,
        )
        .unwrap_err();
        assert!(err.contains("错误项"), "报错应指明是哪一条: {err}");
        assert!(err.contains("NoSuchKey"), "报错应指明是哪个键名: {err}");
    }

    #[test]
    fn 鼠标映射即使被禁用也要装钩子() {
        // 否则托盘把它打开后会不生效
        let cfg = parse(
            r#"
[[mappings]]
enable = false
from = "MouseX1"
to = "Esc"
"#,
        )
        .unwrap();
        assert_eq!(cfg.enabled_count(), 0);
        assert!(cfg.needs_mouse_hook());
    }

    #[test]
    fn 无鼠标映射时不需要鼠标钩子() {
        let cfg = parse(
            r#"
[[mappings]]
from = "Pause"
to = "Insert"
"#,
        )
        .unwrap();
        assert!(!cfg.needs_mouse_hook());
    }
}
