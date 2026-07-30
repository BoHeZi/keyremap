//! 键名 <-> 虚拟键码映射表。
//!
//! 这张表是主程序与 Web 配置工具的共同契约: `--dump-keys` 会把它输出成 JSON,
//! 前端直接消费, 避免两边各维护一份导致命名漂移。
//!
//! 命名原则: 用户看得懂优先, 不暴露 Win32 的 OEM_1 这类内部名。

use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

/// 输入源: 键盘按键或鼠标按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(u16),
    Mouse(MouseButton),
}

/// 鼠标按键。X1/X2 是侧键 (通常 X1=后退, X2=前进)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// 鼠标按键名表。
const MOUSE_TABLE: &[(&str, MouseButton)] = &[
    ("MouseLeft", MouseButton::Left),
    ("MouseRight", MouseButton::Right),
    ("MouseMiddle", MouseButton::Middle),
    ("MouseX1", MouseButton::X1),
    ("MouseX2", MouseButton::X2),
];

/// 键盘按键名表。顺序即 `--dump-keys` 的输出顺序, 按功能分组便于前端分类展示。
#[rustfmt::skip]
pub const KEY_TABLE: &[(&str, u16)] = &[
    // 字母
    ("A", VK_A), ("B", VK_B), ("C", VK_C), ("D", VK_D), ("E", VK_E),
    ("F", VK_F), ("G", VK_G), ("H", VK_H), ("I", VK_I), ("J", VK_J),
    ("K", VK_K), ("L", VK_L), ("M", VK_M), ("N", VK_N), ("O", VK_O),
    ("P", VK_P), ("Q", VK_Q), ("R", VK_R), ("S", VK_S), ("T", VK_T),
    ("U", VK_U), ("V", VK_V), ("W", VK_W), ("X", VK_X), ("Y", VK_Y),
    ("Z", VK_Z),

    // 主键盘数字
    ("0", VK_0), ("1", VK_1), ("2", VK_2), ("3", VK_3), ("4", VK_4),
    ("5", VK_5), ("6", VK_6), ("7", VK_7), ("8", VK_8), ("9", VK_9),

    // 功能键
    ("F1", VK_F1), ("F2", VK_F2), ("F3", VK_F3), ("F4", VK_F4),
    ("F5", VK_F5), ("F6", VK_F6), ("F7", VK_F7), ("F8", VK_F8),
    ("F9", VK_F9), ("F10", VK_F10), ("F11", VK_F11), ("F12", VK_F12),
    ("F13", VK_F13), ("F14", VK_F14), ("F15", VK_F15), ("F16", VK_F16),
    ("F17", VK_F17), ("F18", VK_F18), ("F19", VK_F19), ("F20", VK_F20),
    ("F21", VK_F21), ("F22", VK_F22), ("F23", VK_F23), ("F24", VK_F24),

    // 修饰键。不带 L/R 前缀的是"任意侧", 注入时用左键。
    ("Ctrl", VK_CONTROL), ("LCtrl", VK_LCONTROL), ("RCtrl", VK_RCONTROL),
    ("Alt", VK_MENU), ("LAlt", VK_LMENU), ("RAlt", VK_RMENU),
    ("Shift", VK_SHIFT), ("LShift", VK_LSHIFT), ("RShift", VK_RSHIFT),
    ("LWin", VK_LWIN), ("RWin", VK_RWIN),

    // 编辑与导航
    ("Esc", VK_ESCAPE), ("Tab", VK_TAB), ("CapsLock", VK_CAPITAL),
    ("Space", VK_SPACE), ("Enter", VK_RETURN), ("Backspace", VK_BACK),
    ("Insert", VK_INSERT), ("Delete", VK_DELETE),
    ("Home", VK_HOME), ("End", VK_END),
    ("PageUp", VK_PRIOR), ("PageDown", VK_NEXT),
    ("Left", VK_LEFT), ("Right", VK_RIGHT), ("Up", VK_UP), ("Down", VK_DOWN),
    ("PrintScreen", VK_SNAPSHOT), ("ScrollLock", VK_SCROLL), ("Pause", VK_PAUSE),
    ("Apps", VK_APPS), ("NumLock", VK_NUMLOCK),

    // 符号键 (US 布局下的刻印)
    ("Backquote", VK_OEM_3), ("Minus", VK_OEM_MINUS), ("Equal", VK_OEM_PLUS),
    ("LeftBracket", VK_OEM_4), ("RightBracket", VK_OEM_6), ("Backslash", VK_OEM_5),
    ("Semicolon", VK_OEM_1), ("Quote", VK_OEM_7),
    ("Comma", VK_OEM_COMMA), ("Period", VK_OEM_PERIOD), ("Slash", VK_OEM_2),

    // 小键盘
    ("Numpad0", VK_NUMPAD0), ("Numpad1", VK_NUMPAD1), ("Numpad2", VK_NUMPAD2),
    ("Numpad3", VK_NUMPAD3), ("Numpad4", VK_NUMPAD4), ("Numpad5", VK_NUMPAD5),
    ("Numpad6", VK_NUMPAD6), ("Numpad7", VK_NUMPAD7), ("Numpad8", VK_NUMPAD8),
    ("Numpad9", VK_NUMPAD9),
    ("NumpadAdd", VK_ADD), ("NumpadSubtract", VK_SUBTRACT),
    ("NumpadMultiply", VK_MULTIPLY), ("NumpadDivide", VK_DIVIDE),
    ("NumpadDecimal", VK_DECIMAL),

    // 媒体键
    ("VolumeMute", VK_VOLUME_MUTE), ("VolumeDown", VK_VOLUME_DOWN),
    ("VolumeUp", VK_VOLUME_UP),
    ("MediaNext", VK_MEDIA_NEXT_TRACK), ("MediaPrev", VK_MEDIA_PREV_TRACK),
    ("MediaStop", VK_MEDIA_STOP), ("MediaPlayPause", VK_MEDIA_PLAY_PAUSE),
];

/// 按名字解析输入源, 键盘和鼠标统一入口。配置加载时调用, 不在热路径上。
pub fn input_from_name(name: &str) -> Option<Input> {
    if let Some((_, btn)) = MOUSE_TABLE
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
    {
        return Some(Input::Mouse(*btn));
    }
    vk_from_name(name).map(Input::Key)
}

/// 按名字解析虚拟键码。
pub fn vk_from_name(name: &str) -> Option<u16> {
    KEY_TABLE
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, vk)| *vk)
}

/// 反查键名, 用于 --listen 模式和日志。表只有百余项, 线性扫描足够。
pub fn name_from_vk(vk: u16) -> Option<&'static str> {
    KEY_TABLE.iter().find(|(_, v)| *v == vk).map(|(n, _)| *n)
}

/// 反查鼠标按键名。
pub fn name_from_mouse(btn: MouseButton) -> &'static str {
    MOUSE_TABLE
        .iter()
        .find(|(_, b)| *b == btn)
        .map(|(n, _)| *n)
        .unwrap_or("Mouse?")
}

/// 扩展键判定。
///
/// 这类键在 PS/2 扫描码里带 0xE0 前缀, 注入时必须带 `KEYEVENTF_EXTENDEDKEY`,
/// 否则会被识别成小键盘上的同码位键 —— 典型症状是"映射到 Insert 却变成了数字 0"。
/// 这是自己实现注入时最容易漏掉的一处。
pub fn is_extended_key(vk: u16) -> bool {
    matches!(
        vk,
        VK_RCONTROL
            | VK_RMENU
            | VK_INSERT
            | VK_DELETE
            | VK_HOME
            | VK_END
            | VK_PRIOR
            | VK_NEXT
            | VK_LEFT
            | VK_RIGHT
            | VK_UP
            | VK_DOWN
            | VK_NUMLOCK
            | VK_DIVIDE
            | VK_SNAPSHOT
            | VK_LWIN
            | VK_RWIN
            | VK_APPS
    )
}

/// 把"任意侧"修饰键规范化成具体的左键, 供注入使用。
///
/// `VK_CONTROL`/`VK_MENU`/`VK_SHIFT` 是逻辑键, 直接 SendInput 行为不确定,
/// 必须落成 `VK_LCONTROL` 这类物理键。
pub fn normalize_for_inject(vk: u16) -> u16 {
    match vk {
        VK_CONTROL => VK_LCONTROL,
        VK_MENU => VK_LMENU,
        VK_SHIFT => VK_LSHIFT,
        other => other,
    }
}
