//! 托盘图标与右键菜单。
//!
//! 菜单在配置重载后会整体重建 —— 映射条目本身可能增删, 逐项去 diff 不划算。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use log::{debug, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::hook;

/// 菜单点击后, 主循环需要执行的动作。
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Reload,
    Quit,
}

/// 菜单里各项的 id, 用于把点击事件对回具体功能。
struct Ids {
    toggle: MenuId,
    reload: MenuId,
    open_file: MenuId,
    open_dir: MenuId,
    quit: MenuId,
    /// 下标与配置中 mappings 的下标一一对应
    mappings: Vec<MenuId>,
}

pub struct Tray {
    tray: TrayIcon,
    config_path: PathBuf,
    toggle: CheckMenuItem,
    mapping_items: Vec<CheckMenuItem>,
    ids: Ids,
}

impl Tray {
    pub fn new(config_path: &Path) -> Result<Self, String> {
        let (menu, toggle, mapping_items, ids) = build_menu()?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(load_icon())
            .with_tooltip(tooltip_text())
            .build()
            .map_err(|e| format!("创建托盘图标失败: {e}"))?;

        Ok(Self {
            tray,
            config_path: config_path.to_path_buf(),
            toggle,
            mapping_items,
            ids,
        })
    }

    /// 配置重载后刷新菜单与提示文字。
    pub fn refresh(&mut self) {
        match build_menu() {
            Ok((menu, toggle, items, ids)) => {
                self.tray.set_menu(Some(Box::new(menu)));
                self.toggle = toggle;
                self.mapping_items = items;
                self.ids = ids;
                self.update_tooltip();
                debug!("托盘菜单已刷新");
            }
            Err(e) => warn!("刷新托盘菜单失败: {e}"),
        }
    }

    /// 处理一次菜单点击。
    pub fn on_menu(&mut self, id: &MenuId) -> Action {
        if *id == self.ids.quit {
            return Action::Quit;
        }
        if *id == self.ids.reload {
            return Action::Reload;
        }
        if *id == self.ids.toggle {
            let on = !hook::is_enabled();
            hook::set_enabled(on);
            self.toggle.set_checked(on);
            self.update_tooltip();
            return Action::Nothing;
        }
        if *id == self.ids.open_file {
            open_path(&self.config_path);
            return Action::Nothing;
        }
        if *id == self.ids.open_dir {
            if let Some(dir) = self.config_path.parent() {
                open_path(dir);
            }
            return Action::Nothing;
        }

        // 逐条映射的开关
        if let Some(idx) = self.ids.mappings.iter().position(|m| m == id)
            && let Some(now) = hook::toggle_mapping(idx)
        {
            if let Some(item) = self.mapping_items.get(idx) {
                item.set_checked(now);
            }
            self.update_tooltip();
        }
        Action::Nothing
    }

    fn update_tooltip(&self) {
        if let Err(e) = self.tray.set_tooltip(Some(tooltip_text())) {
            warn!("更新托盘提示失败: {e}");
        }
    }
}

/// 构建整个菜单。返回菜单本体、总开关项、各映射开关项, 以及它们的 id。
fn build_menu() -> Result<(Menu, CheckMenuItem, Vec<CheckMenuItem>, Ids), String> {
    let menu = Menu::new();
    let err = |e| format!("构建菜单失败: {e}");

    let toggle = CheckMenuItem::new("启用映射", true, hook::is_enabled(), None);
    menu.append(&toggle).map_err(err)?;
    menu.append(&PredefinedMenuItem::separator()).map_err(err)?;

    let mut mapping_items = Vec::new();
    let mut mapping_ids = Vec::new();

    if let Some(cfg) = hook::config_snapshot() {
        for m in &cfg.mappings {
            // 菜单项标题: "名称  (Pause -> Insert)", 没写名称时只显示映射本身
            let label = if m.name.is_empty() {
                m.to_string()
            } else {
                format!("{}  ({})", m.name, m)
            };
            let item = CheckMenuItem::new(label, true, m.enable, None);
            menu.append(&item).map_err(err)?;
            mapping_ids.push(item.id().clone());
            mapping_items.push(item);
        }
        if !cfg.mappings.is_empty() {
            menu.append(&PredefinedMenuItem::separator()).map_err(err)?;
        }
    }

    let reload = MenuItem::new("重新加载配置", true, None);
    let open_file = MenuItem::new("打开配置文件", true, None);
    let open_dir = MenuItem::new("打开配置目录", true, None);
    let quit = MenuItem::new("退出", true, None);

    for item in [&reload, &open_file, &open_dir] {
        menu.append(item).map_err(err)?;
    }
    menu.append(&PredefinedMenuItem::separator()).map_err(err)?;
    menu.append(&quit).map_err(err)?;

    let ids = Ids {
        toggle: toggle.id().clone(),
        reload: reload.id().clone(),
        open_file: open_file.id().clone(),
        open_dir: open_dir.id().clone(),
        quit: quit.id().clone(),
        mappings: mapping_ids,
    };

    Ok((menu, toggle, mapping_items, ids))
}

fn tooltip_text() -> String {
    let total = hook::config_snapshot()
        .map(|c| c.enabled_count())
        .unwrap_or(0);
    format!(
        "keyremap-ng — {} ({} 条映射生效)",
        if hook::is_enabled() {
            "已启用"
        } else {
            "已禁用"
        },
        total
    )
}

/// 从 exe 内嵌资源加载图标 (资源名 "id" 定义在 assets/app.rc)。
/// 失败时退回一个纯色图标, 保证托盘一定能显示出来而不是整个程序起不来。
fn load_icon() -> Icon {
    match Icon::from_resource_name("id", None) {
        Ok(icon) => icon,
        Err(e) => {
            warn!("加载内嵌图标失败, 使用兜底图标: {e}");
            let size = 16u32;
            let rgba = (0..size * size)
                .flat_map(|_| [0x2Du8, 0x7D, 0xD2, 0xFF])
                .collect();
            Icon::from_rgba(rgba, size, size).expect("兜底图标必定有效")
        }
    }
}

/// 用系统默认程序打开文件或目录。
fn open_path(path: &Path) {
    let file = to_wide(path.as_os_str());
    let verb = to_wide(OsStr::new("open"));
    unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn to_wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}
