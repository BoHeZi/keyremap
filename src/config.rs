//! 配置的解析与"编译"。
//!
//! 配置文件里写的是键名字符串, 但钩子回调是全系统按键的必经之路,
//! 绝不能在那里做字符串比较。所以加载时一次性把键名解析成虚拟键码、
//! 把组名解析成索引, 运行期只比较整数。键名写错也在这一步就报出来,
//! 而不是等到按下才失败。

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;

use crate::inject::MAX_COMBO;
use crate::keycode::{
    Input, MAX_MODS, input_from_name, mod_from_name, mods_name, name_from_mouse, name_from_vk,
    vk_from_name,
};

/// 未分组映射在菜单里显示的名字。
pub const UNGROUPED_LABEL: &str = "未分组";

/// Web 配置工具的默认地址 —— 仓库自带的 GitHub Pages 部署。
///
/// 之所以能有默认值而不牵扯任何网络代码: 主程序只是把这个地址交给
/// ShellExecuteW 让系统浏览器去开, 自己既不发请求也不解析响应。
pub const DEFAULT_WEB_URL: &str = "https://huanfeng.github.io/keyremap/";

// ---------- 文件里的原始结构 ----------

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    name: String,
    /// Web 配置工具的地址。留空则用 [`DEFAULT_WEB_URL`]。
    /// 自行部署到更快的服务时改这里。
    #[serde(default)]
    web_url: String,
    /// 组的启用状态, 整段可选。没列出的组默认启用,
    /// 所以只想分组、不想预设开关时可以完全不写这一段。
    #[serde(default)]
    groups: HashMap<String, bool>,
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
    /// 所属组, 不写则归入未分组。组不需要预先声明。
    #[serde(default)]
    group: String,
    /// 输入源。`from = "Pause"` 或 `from = ["Ctrl", "E"]` (末位是触发键)。
    from: RawKeys,
    to: RawKeys,
}

/// 键位既可以写成 `"Insert"`, 也可以写成 `["Ctrl", "W"]`。
/// `from` 和 `to` 共用这个形状, 用户就不用记两套写法。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawKeys {
    Single(String),
    Combination(Vec<String>),
}

impl RawKeys {
    /// 统一成切片视角, 省得每处都 match 一遍。
    fn parts(&self) -> &[String] {
        match self {
            RawKeys::Single(s) => std::slice::from_ref(s),
            RawKeys::Combination(v) => v,
        }
    }
}

fn default_true() -> bool {
    true
}

// ---------- 编译后的运行期结构 ----------

/// 一个映射组。组的顺序由映射中首次出现的次序决定。
#[derive(Debug, Clone)]
pub struct Group {
    /// 组名。空字符串代表"未分组"。
    pub name: String,
    pub enable: bool,
}

impl Group {
    /// 菜单里显示的名字, 未分组显示成固定文案。
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            UNGROUPED_LABEL
        } else {
            &self.name
        }
    }
}

/// 一条编译好的映射。热路径上只会读 `enable` / `group` / `from` / `to`。
#[derive(Debug, Clone)]
pub struct Mapping {
    pub name: String,
    pub comment: String,
    pub enable: bool,
    /// 所属组在 [`Config::groups`] 中的下标。
    /// 存索引而不是组名: 钩子回调里不能做字符串比较。
    pub group: usize,
    /// 触发键。写成 `["Ctrl", "E"]` 时这里存的是末位的 `E`。
    ///
    /// 修饰键单独放在 [`Mapping::from_mods`] 里, 而不是并进一个序列 ——
    /// 这样钩子回调的第一步仍然只是一次整数比较, 绝大多数按键在这里就被排除了,
    /// 根本不会去查修饰键状态。
    pub from: Input,
    /// 触发前必须按住的修饰键位掩码, 0 表示不要求。见 `keycode::MOD_*`。
    pub from_mods: u16,
    /// 目标按键序列。长度为 1 表示单键映射, 大于 1 表示组合键。
    pub to: Vec<u16>,
}

impl Mapping {
    /// 是否是组合键映射。单键与组合键的触发时机不同, 见 `hook::dispatch`。
    pub fn is_combo(&self) -> bool {
        self.to.len() > 1
    }

    /// 输入源是否带修饰键 (即 `from` 写成了数组形式)。
    pub fn has_mods(&self) -> bool {
        self.from_mods != 0
    }

    /// 菜单项与日志里显示的标题。没写 name 时退回映射本身的描述。
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            self.to_string()
        } else {
            format!("{}  ({})", self.name, self)
        }
    }

    /// 输入源的显示名。
    ///
    /// 不叫 from_name 是因为 `from_*` 在 Rust 里通常表示构造函数, clippy 会提醒。
    pub fn input_name(&self) -> String {
        let trigger = match self.from {
            Input::Key(vk) => name_from_vk(vk).unwrap_or("?"),
            Input::Mouse(btn) => name_from_mouse(btn),
        };
        if self.from_mods == 0 {
            trigger.to_string()
        } else {
            format!("{} + {trigger}", mods_name(self.from_mods))
        }
    }
}

impl fmt::Display for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let from = self.input_name();
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
    /// 配置里指定的 Web 工具地址, 空表示用默认值。取值请走 [`Config::web_url`]。
    pub web_url_override: String,
    pub groups: Vec<Group>,
    /// 按配置文件里的声明顺序存放。菜单、`--dump`、以及托盘按下标切换开关
    /// 都依赖这个顺序, 所以它不能被重排。
    pub mappings: Vec<Mapping>,
    /// 钩子回调遍历 [`Config::mappings`] 时使用的下标顺序, 见 [`match_order`]。
    pub order: Vec<u32>,
}

impl Config {
    /// Web 配置工具的地址: 配置里写了就用它, 否则用默认的 GitHub Pages 地址。
    pub fn web_url(&self) -> &str {
        if self.web_url_override.trim().is_empty() {
            DEFAULT_WEB_URL
        } else {
            self.web_url_override.trim()
        }
    }

    /// 这条映射当前是否真正生效: 自身启用**且**所属组也启用。
    ///
    /// 热路径上调用, 所以是一次数组索引而非字符串查找。
    pub fn is_active(&self, m: &Mapping) -> bool {
        m.enable && self.groups.get(m.group).map(|g| g.enable).unwrap_or(true)
    }

    /// 配置里是否存在鼠标映射。
    ///
    /// 没有的话就不安装鼠标钩子 —— `WH_MOUSE_LL` 会收到全部鼠标移动事件,
    /// 鼠标动一下就进一次回调, 这是常态下最大的一笔无谓开销。
    ///
    /// 注意这里**不看启用状态**: 托盘菜单可以随时把某条鼠标映射或整个组打开,
    /// 若按启用状态决定是否装钩子, 那条映射打开后会不生效。
    pub fn needs_mouse_hook(&self) -> bool {
        self.mappings
            .iter()
            .any(|m| matches!(m.from, Input::Mouse(_)))
    }

    /// 实际生效的映射条数。
    pub fn active_count(&self) -> usize {
        self.mappings.iter().filter(|m| self.is_active(m)).count()
    }

    /// 某个组内实际生效的条数与总条数, 用于菜单上的 (2/3) 这种提示。
    pub fn group_counts(&self, index: usize) -> (usize, usize) {
        let total = self.mappings.iter().filter(|m| m.group == index).count();
        let on = self
            .mappings
            .iter()
            .filter(|m| m.group == index && self.is_active(m))
            .count();
        (on, total)
    }

    /// 按组归拢映射下标, 供菜单按组分区展示。
    pub fn mappings_in_group(&self, index: usize) -> impl Iterator<Item = (usize, &Mapping)> {
        self.mappings
            .iter()
            .enumerate()
            .filter(move |(_, m)| m.group == index)
    }

    /// 找出输入源相同的生效映射, 返回 (先命中的下标, 被遮盖的下标)。
    ///
    /// 匹配是先到先得 —— `hook::dispatch` 命中第一条就返回, 后面同 `from` 的
    /// 映射永远不会被执行, 而程序此前对此没有任何提示, 这种静默失效很难排查。
    ///
    /// 只检查**同时生效**的条目: 用两个互斥的组切换同一个键的不同映射
    /// (比如"游戏模式"和"办公模式") 是合理用法, 只要它们不同时启用就不算冲突。
    pub fn find_conflicts(&self) -> Vec<(usize, usize)> {
        let active: Vec<usize> = self
            .mappings
            .iter()
            .enumerate()
            .filter(|(_, m)| self.is_active(m))
            .map(|(i, _)| i)
            .collect();

        let mut conflicts = Vec::new();
        for (a, &i) in active.iter().enumerate() {
            for &j in &active[a + 1..] {
                // 修饰键不同就不算撞车: `E` 和 `Ctrl+E` 是两个不同的输入源
                if self.mappings[i].from == self.mappings[j].from
                    && self.mappings[i].from_mods == self.mappings[j].from_mods
                {
                    conflicts.push((i, j));
                }
            }
        }
        conflicts
    }
}

/// 把配置里的冲突以警告形式打进日志。加载与热重载都会走这里。
pub fn warn_conflicts(cfg: &Config) {
    for (first, shadowed) in cfg.find_conflicts() {
        log::warn!(
            "配置冲突: 「{}」已占用 {}, 「{}」不会生效",
            cfg.mappings[first].label(),
            cfg.mappings[first].input_name(),
            cfg.mappings[shadowed].label(),
        );
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

    let mut groups: Vec<Group> = Vec::new();
    let mut mappings = Vec::with_capacity(raw.mappings.len());

    for (i, rm) in raw.mappings.into_iter().enumerate() {
        // 报错时带上条目序号和名字, 方便定位是哪一条写错了
        let label = if rm.name.is_empty() {
            format!("第 {} 条映射", i + 1)
        } else {
            format!("映射 \"{}\"", rm.name)
        };

        let (from, from_mods) = parse_from(rm.from.parts(), &label)?;

        let to_names = rm.to.parts();

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
        for name in to_names {
            let vk = vk_from_name(name).ok_or_else(|| {
                format!("{label}: 未知的按键名 \"{name}\" (输出目前只支持键盘按键)")
            })?;
            to.push(vk);
        }

        // 组按首次出现的顺序登记, 不需要预先声明。
        // 启用状态取 [groups] 里的设置, 没写就默认启用。
        let group = match groups.iter().position(|g| g.name == rm.group) {
            Some(idx) => idx,
            None => {
                let enable = raw.groups.get(&rm.group).copied().unwrap_or(true);
                groups.push(Group {
                    name: rm.group.clone(),
                    enable,
                });
                groups.len() - 1
            }
        };

        mappings.push(Mapping {
            name: rm.name,
            comment: rm.comment,
            enable: rm.enable,
            group,
            from,
            from_mods,
            to,
        });
    }

    let order = match_order(&mappings);
    Ok(Config {
        name: raw.name,
        web_url_override: raw.web_url,
        groups,
        mappings,
        order,
    })
}

/// 解析 `from`: 末位是触发键, 前面的都必须是修饰键。
///
/// `from = "Pause"` 与 `from = ["Pause"]` 等价, 都表示不要求修饰键。
fn parse_from(parts: &[String], label: &str) -> Result<(Input, u16), String> {
    let Some((trigger_name, mod_names)) = parts.split_last() else {
        return Err(format!("{label}: from 不能为空"));
    };

    if mod_names.len() > MAX_MODS {
        return Err(format!(
            "{label}: 修饰键最多 {MAX_MODS} 个, 实际 {}",
            mod_names.len()
        ));
    }

    let mut mods = 0u16;
    for name in mod_names {
        let bit = mod_from_name(name).ok_or_else(|| {
            format!(
                "{label}: \"{name}\" 不是修饰键。数组形式的 from 里, \
                 除最后一个触发键外只能写 Ctrl / Shift / Alt / Win"
            )
        })?;
        if mods & bit != 0 {
            return Err(format!("{label}: 修饰键 \"{name}\" 重复了"));
        }
        mods |= bit;
    }

    // 触发键自己不能又是修饰键: `["Ctrl", "Shift"]` 这种写法没有明确含义 ——
    // 到底是"按住 Ctrl 时按 Shift"还是"同时按住两个"? 直接拒掉, 不猜。
    if mod_from_name(trigger_name).is_some() {
        return Err(format!(
            "{label}: 触发键不能是修饰键 (\"{trigger_name}\"), \
             修饰键只能写在前面"
        ));
    }

    let from = input_from_name(trigger_name)
        .ok_or_else(|| format!("{label}: 未知的按键名 \"{trigger_name}\""))?;
    Ok((from, mods))
}

/// 钩子回调遍历映射时使用的顺序: **修饰键多的排在前面**。
///
/// 同时配了 `E -> D` 和 `Ctrl+E -> Backspace` 时, 按住 Ctrl 应当走后者。
/// 若按声明顺序匹配, 谁写在前面谁生效 —— 那种行为没法向用户解释。
///
/// 只影响匹配, 不影响显示: 菜单和 `--dump` 仍按配置文件里的顺序,
/// 否则用户会发现界面上的条目莫名其妙地重排了。
/// 用稳定排序, 所以修饰键数量相同的几条仍然保持"先到先得"。
fn match_order(mappings: &[Mapping]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..mappings.len() as u32).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(mappings[i as usize].from_mods.count_ones()));
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keycode::{MOD_CTRL, MOD_RALT, MOD_SHIFT, MouseButton};

    const SAMPLE: &str = r#"
name = "测试配置"

[groups]
"编辑增强" = false

[[mappings]]
name = "Pause转Insert"
from = "Pause"
to = "Insert"

[[mappings]]
name = "侧键转Ctrl+W"
group = "浏览器"
from = "MouseX2"
to = ["Ctrl", "W"]

[[mappings]]
name = "组内被禁用"
group = "编辑增强"
from = "F13"
to = "F14"

[[mappings]]
name = "自身禁用"
enable = false
from = "D"
to = "E"
"#;

    #[test]
    fn 解析单键与组合键() {
        let cfg = parse(SAMPLE).expect("应当解析成功");
        assert_eq!(cfg.mappings.len(), 4);
        assert!(!cfg.mappings[0].is_combo());
        assert!(cfg.mappings[1].is_combo());
        assert_eq!(cfg.mappings[1].to.len(), 2);
    }

    // ---- 组合键作为输入源 ----

    /// 只取第一条映射, 省得每个用例都写一遍解包。
    fn one(toml: &str) -> Mapping {
        parse(toml).expect("应当解析成功").mappings.remove(0)
    }

    #[test]
    fn 数组形式的from末位是触发键() {
        let m = one(r#"[[mappings]]
from = ["Ctrl", "E"]
to = "Backspace""#);
        assert_eq!(m.from, Input::Key(vk_from_name("E").unwrap()));
        assert_eq!(m.from_mods, MOD_CTRL);
        assert!(m.has_mods());
    }

    #[test]
    fn 多个修饰键合并成位掩码() {
        let m = one(r#"[[mappings]]
from = ["Ctrl", "Shift", "E"]
to = "Backspace""#);
        assert_eq!(m.from_mods, MOD_CTRL | MOD_SHIFT);
        // 显示名按固定顺序, 与用户写的顺序无关
        assert_eq!(m.input_name(), "Ctrl + Shift + E");
    }

    #[test]
    fn 修饰键顺序不影响结果() {
        let a = one(r#"[[mappings]]
from = ["Shift", "Ctrl", "E"]
to = "Backspace""#);
        let b = one(r#"[[mappings]]
from = ["Ctrl", "Shift", "E"]
to = "Backspace""#);
        assert_eq!(a.from_mods, b.from_mods);
        assert_eq!(a.input_name(), b.input_name());
    }

    #[test]
    fn 单元素数组等价于字符串写法() {
        let a = one("[[mappings]]\nfrom = [\"Pause\"]\nto = \"Insert\"");
        let b = one("[[mappings]]\nfrom = \"Pause\"\nto = \"Insert\"");
        assert_eq!(a.from, b.from);
        assert_eq!(a.from_mods, 0);
        assert!(!a.has_mods());
    }

    #[test]
    fn 触发键可以是鼠标键() {
        let m = one(r#"[[mappings]]
from = ["Ctrl", "MouseX2"]
to = ["Ctrl", "Shift", "T"]"#);
        assert_eq!(m.from, Input::Mouse(MouseButton::X2));
        assert_eq!(m.from_mods, MOD_CTRL);
        assert_eq!(m.input_name(), "Ctrl + MouseX2");
    }

    #[test]
    fn 可以指定左右某一侧() {
        let m = one(r#"[[mappings]]
from = ["RAlt", "E"]
to = "Backspace""#);
        assert_eq!(m.from_mods, MOD_RALT);
        assert_eq!(m.input_name(), "RAlt + E");
    }

    #[test]
    fn 通用写法与分侧写法是不同的要求() {
        let generic = one("[[mappings]]\nfrom = [\"Ctrl\", \"E\"]\nto = \"Backspace\"");
        let left = one("[[mappings]]\nfrom = [\"LCtrl\", \"E\"]\nto = \"Backspace\"");
        assert_ne!(generic.from_mods, left.from_mods);
        // 触发键相同但要求不同, 所以不算冲突
        let cfg = parse(
            r#"
[[mappings]]
from = ["Ctrl", "E"]
to = "Backspace"

[[mappings]]
from = ["LCtrl", "E"]
to = "Delete"
"#,
        )
        .unwrap();
        assert!(cfg.find_conflicts().is_empty());
    }

    #[test]
    fn 非修饰键写在前面要报错() {
        let e = parse("[[mappings]]\nfrom = [\"A\", \"E\"]\nto = \"Backspace\"").unwrap_err();
        assert!(e.contains("不是修饰键"), "错误信息要指出问题: {e}");
    }

    #[test]
    fn 触发键是修饰键要报错() {
        // ["Ctrl", "Shift"] 含义不明: 是"按住 Ctrl 时按 Shift"还是同时按住?
        // 与其猜, 不如直接拒掉
        let e =
            parse("[[mappings]]\nfrom = [\"Ctrl\", \"Shift\"]\nto = \"Backspace\"").unwrap_err();
        assert!(e.contains("触发键不能是修饰键"), "{e}");
    }

    #[test]
    fn 重复的修饰键要报错() {
        let e = parse("[[mappings]]\nfrom = [\"Ctrl\", \"Ctrl\", \"E\"]\nto = \"Backspace\"")
            .unwrap_err();
        assert!(e.contains("重复"), "{e}");
    }

    #[test]
    fn 修饰键多的排在匹配顺序前面() {
        // 声明顺序故意反着写: 无修饰的在前
        let cfg = parse(
            r#"
[[mappings]]
name = "无修饰"
from = "E"
to = "D"

[[mappings]]
name = "带一个"
from = ["Ctrl", "E"]
to = "Backspace"

[[mappings]]
name = "带两个"
from = ["Ctrl", "Shift", "E"]
to = "Delete"
"#,
        )
        .unwrap();

        // 显示顺序保持配置文件里的样子
        assert_eq!(cfg.mappings[0].name, "无修饰");
        assert_eq!(cfg.mappings[2].name, "带两个");

        // 匹配顺序则是修饰键多的优先, 否则 `E -> D` 会把 `Ctrl+E` 挡住
        let names: Vec<&str> = cfg
            .order
            .iter()
            .map(|&i| cfg.mappings[i as usize].name.as_str())
            .collect();
        assert_eq!(names, vec!["带两个", "带一个", "无修饰"]);
    }

    #[test]
    fn 修饰键数量相同的仍按声明顺序() {
        let cfg = parse(
            r#"
[[mappings]]
name = "先"
from = ["Ctrl", "E"]
to = "Backspace"

[[mappings]]
name = "后"
from = ["Alt", "R"]
to = "Delete"
"#,
        )
        .unwrap();
        let names: Vec<&str> = cfg
            .order
            .iter()
            .map(|&i| cfg.mappings[i as usize].name.as_str())
            .collect();
        assert_eq!(names, vec!["先", "后"], "稳定排序应保持先到先得");
    }

    #[test]
    fn 修饰键不同不算输入源冲突() {
        let cfg = parse(
            r#"
[[mappings]]
from = "E"
to = "D"

[[mappings]]
from = ["Ctrl", "E"]
to = "Backspace"
"#,
        )
        .unwrap();
        assert!(cfg.find_conflicts().is_empty(), "E 和 Ctrl+E 是两个输入源");
    }

    #[test]
    fn 修饰键相同才算冲突() {
        let cfg = parse(
            r#"
[[mappings]]
from = ["Ctrl", "E"]
to = "Backspace"

[[mappings]]
from = ["Ctrl", "E"]
to = "Delete"
"#,
        )
        .unwrap();
        assert_eq!(cfg.find_conflicts(), vec![(0, 1)]);
    }

    #[test]
    fn 鼠标输入被正确识别() {
        let cfg = parse(SAMPLE).unwrap();
        assert_eq!(cfg.mappings[1].from, Input::Mouse(MouseButton::X2));
        assert!(cfg.needs_mouse_hook());
    }

    #[test]
    fn 组按首次出现顺序登记且未分组也算一组() {
        let cfg = parse(SAMPLE).unwrap();
        // 第一条没写 group, 所以未分组是 0 号
        assert_eq!(cfg.groups[0].name, "");
        assert_eq!(cfg.groups[0].display_name(), UNGROUPED_LABEL);
        assert_eq!(cfg.groups[1].name, "浏览器");
        assert_eq!(cfg.groups[2].name, "编辑增强");
    }

    #[test]
    fn 未声明的组默认启用() {
        let cfg = parse(SAMPLE).unwrap();
        assert!(
            cfg.groups[1].enable,
            "浏览器组没在 [groups] 里列出, 应默认启用"
        );
    }

    #[test]
    fn 组被禁用时组内映射不生效() {
        let cfg = parse(SAMPLE).unwrap();
        let m = &cfg.mappings[2];
        assert!(m.enable, "映射自身是启用的");
        assert!(!cfg.is_active(m), "但所属组被禁用, 因此不应生效");
    }

    #[test]
    fn 生效条数同时考虑组开关与自身开关() {
        let cfg = parse(SAMPLE).unwrap();
        // Pause 生效; 浏览器组的生效; 编辑增强被组禁用; 第四条自身禁用
        assert_eq!(cfg.active_count(), 2);
    }

    #[test]
    fn 组内计数只统计本组() {
        let cfg = parse(SAMPLE).unwrap();
        assert_eq!(cfg.group_counts(1), (1, 1), "浏览器组 1 条且生效");
        assert_eq!(cfg.group_counts(2), (0, 1), "编辑增强组 1 条但组被关掉");
    }

    #[test]
    fn 无组配置照旧可用() {
        // 完全不写 [groups] 和 group 字段, 应当和以前一样可用
        let cfg = parse(
            r#"
[[mappings]]
from = "Pause"
to = "Insert"
"#,
        )
        .unwrap();
        assert_eq!(cfg.groups.len(), 1);
        assert_eq!(cfg.active_count(), 1);
        assert!(!cfg.needs_mouse_hook());
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
        assert_eq!(cfg.active_count(), 0);
        assert!(cfg.needs_mouse_hook());
    }

    #[test]
    fn 未指定时用默认的web地址() {
        let cfg = parse(
            r#"
[[mappings]]
from = "Pause"
to = "Insert"
"#,
        )
        .unwrap();
        assert_eq!(cfg.web_url(), DEFAULT_WEB_URL);
    }

    #[test]
    fn 配置里的web地址优先且忽略两侧空白() {
        let cfg = parse(
            r#"
web_url = "  https://my.pages.dev/  "

[[mappings]]
from = "Pause"
to = "Insert"
"#,
        )
        .unwrap();
        assert_eq!(cfg.web_url(), "https://my.pages.dev/");
    }

    #[test]
    fn 空白的web地址退回默认值() {
        let cfg = parse(
            r#"
web_url = "   "

[[mappings]]
from = "Pause"
to = "Insert"
"#,
        )
        .unwrap();
        assert_eq!(cfg.web_url(), DEFAULT_WEB_URL);
    }

    #[test]
    fn 同时生效的重复输入源算冲突() {
        let cfg = parse(
            r#"
[[mappings]]
name = "第一条"
from = "Pause"
to = "Insert"

[[mappings]]
name = "被遮盖"
from = "Pause"
to = "Delete"
"#,
        )
        .unwrap();
        let c = cfg.find_conflicts();
        assert_eq!(c, vec![(0, 1)], "后一条永远不会被匹配到");
    }

    #[test]
    fn 分属互斥组的相同输入源不算冲突() {
        // 用两个组切换同一个键的不同映射是合理用法
        let cfg = parse(
            r#"
[groups]
"办公" = false

[[mappings]]
group = "游戏"
from = "Pause"
to = "Insert"

[[mappings]]
group = "办公"
from = "Pause"
to = "Delete"
"#,
        )
        .unwrap();
        assert!(
            cfg.find_conflicts().is_empty(),
            "两组不同时生效, 不该报冲突"
        );
    }

    #[test]
    fn 自身禁用的条目不参与冲突判定() {
        let cfg = parse(
            r#"
[[mappings]]
from = "Pause"
to = "Insert"

[[mappings]]
enable = false
from = "Pause"
to = "Delete"
"#,
        )
        .unwrap();
        assert!(cfg.find_conflicts().is_empty());
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
}
