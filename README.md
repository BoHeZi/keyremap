# keyremap-ng

Windows 平台的键盘 / 鼠标按键重映射工具。TOML 声明式配置，托盘常驻，改完配置自动生效。

单文件绿色运行，无需安装，无需运行时依赖，release 体积约 900 KB。

## 这个工具适合谁

先说清楚定位，省得你走弯路：

| | 键盘重映射 | **鼠标按键映射** | 声明式配置 | 图形配置界面 | 体积 |
|---|---|---|---|---|---|
| AutoHotkey | ✅ | ✅ | ❌ 需写脚本 | ❌ | 中 |
| PowerToys Keyboard Manager | ✅ | ❌ **不支持** | ✅ | ✅ | 极重 |
| **keyremap-ng** | ✅ | ✅ | ✅ | 计划中 | 极小 |

- **想要功能无上限**（条件判断、按窗口区分、宏、任意脚本逻辑）→ 用 AutoHotkey，本项目没有胜算。
- **只需简单的键→键映射，且不介意装 PowerToys** → 用 PowerToys Keyboard Manager，微软官方维护。
- **需要把鼠标侧键映射成快捷键，又不想写脚本** → 这正是本项目存在的理由。PowerToys 做不到鼠标，AHK 要写脚本。

另一个不那么显眼但真实的理由：键盘钩子在技术上等同于键盘记录器。本项目全部实现约 1200 行，可以逐行读完，而 AutoHotkey 是几十万行 C++。在公司电脑或安全敏感环境里，这有时是决定性的。

## 快速开始

1. 下载 release 里的 `keyremap-ng.exe` 与 `keyremap.toml`，放在同一目录
2. 按需编辑 `keyremap.toml`
3. 双击运行 —— 没有控制台窗口，托盘出现图标即已生效

不确定某个键叫什么名字，用**托盘菜单 → 按键监听**打开一个监听窗口，按下它看输出。命令行等价形式：

```
keyremap-ng.exe --listen
```

开机自启：**托盘菜单 → 开机自启动**，勾上即可（写 `HKCU\...\Run`，不需要管理员权限）。

同一个 exe 只能运行一个实例，但把整个目录复制一份到别处，就能带着各自的配置同时跑多份。

## 配置

配置文件默认是 exe 同目录下的 `keyremap.toml`，也可以用 `-c` 指定。

**保存即生效** —— 程序监听配置文件变化，保存后自动重载并弹出气泡提示。配置写错也不要紧：解析失败时会保留上一份配置继续运行，不会让映射整体失效。

### 单键映射

```toml
[[mappings]]
name = "Pause转Insert"        # 可选，显示在托盘菜单和日志里
comment = "暂停键改成 Insert"  # 可选，纯注释
from = "Pause"
to = "Insert"
```

单键映射会保留长按自动重复的手感。

### 组合键映射

`to` 写成数组即为组合键：

```toml
[[mappings]]
name = "鼠标侧键转Ctrl+W"
from = "MouseX2"
to = ["Ctrl", "W"]
```

组合键在按键**抬起**时触发一次。这是有意的：若在按下时触发，长按会被系统的按键重复反复触发，对 `Ctrl+W` 这类操作意味着一口气关掉一串标签页。

组合键最多 8 个键，用一次原子的 `SendInput` 发出，中途不会被其他输入打断。

### 临时禁用某条映射

```toml
[[mappings]]
enable = false    # 默认为 true
from = "D"
to = "E"
```

也可以在托盘菜单里逐条勾选切换（不写回文件，重载后回到文件里的状态）。

### 分组

给映射加 `group` 就能分组，**组不需要预先声明**。托盘菜单里可以按组整体开关，不必逐条点。

```toml
[[mappings]]
group = "浏览器"
from = "MouseX2"
to = ["Ctrl", "W"]

[[mappings]]
group = "浏览器"
from = "MouseX1"
to = ["Ctrl", "T"]

[[mappings]]        # 不写 group 的归入"未分组"
from = "Pause"
to = "Insert"
```

想让某组默认关着，加一段 `[groups]`。**这一段整体可选**，没列出的组默认启用：

```toml
[groups]
"文本编辑" = false
```

组的显示顺序由映射中首次出现的次序决定。一条映射真正生效需要**自身启用且所属组也启用** —— `--dump` 会用 `grp` 标出"自身开着但组被关掉"的情况。

### 反向映射可以同时启用

```toml
[[mappings]]
from = "D"
to = "E"

[[mappings]]
from = "E"
to = "D"
```

D 和 E 互换不会死循环 —— 注入的按键带标记，钩子识别后直接放行。

### 可用键名

用 `keyremap-ng.exe --dump-keys` 可以输出完整的 JSON 列表。键名不区分大小写。

| 类别 | 键名 |
|---|---|
| 字母 | `A` ~ `Z` |
| 数字 | `0` ~ `9` |
| 功能键 | `F1` ~ `F24` |
| 修饰键 | `Ctrl` `LCtrl` `RCtrl` `Alt` `LAlt` `RAlt` `Shift` `LShift` `RShift` `LWin` `RWin` |
| 编辑导航 | `Esc` `Tab` `CapsLock` `Space` `Enter` `Backspace` `Insert` `Delete` `Home` `End` `PageUp` `PageDown` `Left` `Right` `Up` `Down` `PrintScreen` `ScrollLock` `Pause` `Apps` `NumLock` |
| 符号 | `Backquote` `Minus` `Equal` `LeftBracket` `RightBracket` `Backslash` `Semicolon` `Quote` `Comma` `Period` `Slash` |
| 小键盘 | `Numpad0` ~ `Numpad9` `NumpadAdd` `NumpadSubtract` `NumpadMultiply` `NumpadDivide` `NumpadDecimal` |
| 媒体键 | `VolumeMute` `VolumeDown` `VolumeUp` `MediaNext` `MediaPrev` `MediaStop` `MediaPlayPause` |
| 鼠标 | `MouseLeft` `MouseRight` `MouseMiddle` `MouseX1` `MouseX2` |

`MouseX1` / `MouseX2` 是鼠标侧键，通常 X1 是后退、X2 是前进，但不同鼠标可能相反，用 `--listen` 确认。

不带 `L`/`R` 前缀的修饰键（`Ctrl` `Alt` `Shift`）作为输入时匹配任意一侧，作为输出时注入左侧键。

鼠标按键目前只能作为输入源，`to` 里只支持键盘按键。

## 图形配置工具

`web/` 目录是一个纯静态的配置页面：按键捕获、组合键勾选、分组管理、冲突提示、TOML 预览。
部署到任意静态托管（Cloudflare Pages / GitHub Pages）即可，**主程序本身不含任何网络代码**。

- Chrome / Edge：可以直接打开并保存 `keyremap.toml`，文件授权会被记住
- 其他浏览器：编辑后复制 TOML 粘贴进配置文件

两种方式保存后主程序都会自动重载。键名表由 `--dump-keys` 导出，与主程序同源，CI 会校验二者一致。

详见 [web/README.md](web/README.md)。

## 托盘菜单

右键（或左键）托盘图标：

```
[✓] 启用映射                    总开关，不卸载钩子，切换是即时的
─────────────
    映射设置 (2/4)  ▸           映射条数会变，所以收进子菜单
    按键监听...                 另开一个窗口显示按下的键名
─────────────
    重新加载配置
    打开配置文件                用系统默认编辑器打开
    打开配置目录
─────────────
[✓] 开机自启动
    以管理员身份重启            已是管理员时显示为灰色状态
─────────────
    退出
```

"映射设置"子菜单里，组标题本身就是组开关，组内映射缩进显示；组被关掉时组内各项一并置灰：

```
[✓] 【浏览器】  2/2
    [✓]     关闭标签页  (MouseX2 -> Ctrl + W)
    [✓]     新建标签页  (MouseX1 -> Ctrl + T)
─────────────
[ ] 【文本编辑】  0/2            组关掉了
    [✓]     D转E  (D -> E)      置灰，不可点
    [✓]     E转D  (E -> D)
```

没用分组时不会出现组标题这一层，菜单里直接就是映射列表。

一级菜单的项目是固定的，不随映射条数变化 —— 这样"退出"这类常用项的位置不会漂移。

## 命令行参数

程序是 GUI 子系统的，双击不会有黑窗口；需要输出时会自动附加控制台 —— 从终端运行就附到当前终端，双击运行则新开一个窗口。

| 参数 | 说明 |
|---|---|
| `-c, --config <PATH>` | 指定配置文件，默认为 exe 同目录的 `keyremap.toml` |
| `-l, --listen` | 监听模式，只打印按下的键名，不做任何映射 |
| `--dump` | 按组打印已加载的映射后退出 |
| `--dump-keys` | 输出全部可用键名（JSON）后退出 |
| `-o, --output <FILE>` | 配合上面两个：把输出写入文件 |
| `--logfile` | 把日志写入 exe 同目录的 `keyremap.log` |
| `-v`, `-vv` | 提高日志级别，并自动附加控制台显示日志 |

**要落盘请用 `-o` 而不是 shell 重定向**：GUI 程序不会被 shell 等待，`--dump-keys > keys.json` 会因为管道提前关闭而写不进去。

同一个 exe 只允许运行一个实例 —— 多个实例各装一套钩子会互相干扰，出现"已禁用却仍在生效"这类现象。锁的粒度是**程序路径**，所以复制到别处的另一份可以独立运行。`--dump` 和 `--listen` 不受此限制，方便在后台运行时排查。

## 已知限制

- **目标窗口以管理员权限运行时映射无效**。Windows 的 UIPI 机制会拦掉低权限进程的输入注入 —— 用托盘菜单里的"以管理员身份重启"可以解决。UAC 安全桌面和部分反作弊游戏则任何用户态方案都覆盖不到，那需要内核级驱动。
- **"开机自启" 与 "管理员权限" 不能叠加**。`HKCU\Run` 启动的进程不会提权，这是 Windows 的设计。两者都要的话得在"任务计划程序"里手建一个以最高权限运行的任务。
- **`from` 只支持单个键**，组合键还不能作为输入源 —— 也就是说 `Ctrl+E → Backspace` 目前写不出来。反向（单键 → 组合键）是支持的。
- `to` 暂不支持鼠标按键输出。
- 仅支持 Windows。实现直接调用 Win32 API，没有跨平台打算。

## 构建

```
cargo build --release
```

需要 Rust 1.85+（edition 2024）。产物在 `target/release/keyremap-ng.exe`。

## 实现说明

如果你打算读代码或者做类似的东西，几个可能有用的点：

- 键盘和鼠标用 `SetWindowsHookExW` 装 `WH_KEYBOARD_LL` / `WH_MOUSE_LL` 低级钩子。这是用户态下唯一能**拦截**并改写按键的方式，AutoHotkey 和 PowerToys 也都是这套。`RegisterHotKey` 不能改键，Raw Input 只能观察不能拦截。
- 注入事件的 `dwExtraInfo` 打上标记，钩子回调第一件事就是检查它并放行自己发出的事件。没有这一步，反向映射会无限回环。AutoHotkey 用的是同一手法。
- 钩子回调里不做任何耗时操作：不 sleep、不做字符串比较、不调 `ToUnicodeEx`。低级钩子回调超过 `LowLevelHooksTimeout`（默认 300 ms）会被系统**静默摘掉**，程序不崩但从此失效。组合键因此用单次 `SendInput` 原子发送，而不是逐个发再 sleep。
- 配置在加载时就"编译"成虚拟键码，热路径上只比较整数。
- 键盘钩子、托盘窗口消息、配置重载通知共用同一个消息循环，不额外开线程 —— 低级钩子本来就需要消息泵，托盘图标也需要窗口，两者天然可以合并。
- 配置里没有鼠标映射时不安装鼠标钩子。`WH_MOUSE_LL` 会收到全部鼠标移动事件，鼠标一动就进一次回调，这是常态下最大的一笔无谓开销。

## License

MIT
