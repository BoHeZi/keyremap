# keyremap

Windows 平台的键盘 / 鼠标按键重映射工具。TOML 声明式配置，托盘常驻，改完配置自动生效。

单文件运行，无运行时依赖，release 体积约 1 MB。可以 scoop 安装，也可以解压即用。

## 这个工具适合谁

先说清楚定位，省得你走弯路：

| | 键盘重映射 | **鼠标按键映射** | 声明式配置 | 图形配置界面 | 体积 |
|---|---|---|---|---|---|
| AutoHotkey | ✅ | ✅ | ❌ 需写脚本 | ❌ | 中 |
| PowerToys Keyboard Manager | ✅ | ❌ **不支持** | ✅ | ✅ | 极重 |
| **keyremap** | ✅ | ✅ | ✅ | ✅ 网页版 | 极小 |

- **想要功能无上限**（条件判断、按窗口区分、宏、任意脚本逻辑）→ 用 AutoHotkey，本项目没有胜算。
- **只需简单的键→键映射，且不介意装 PowerToys** → 用 PowerToys Keyboard Manager，微软官方维护。
- **需要把鼠标侧键映射成快捷键，又不想写脚本** → 这正是本项目存在的理由。PowerToys 做不到鼠标，AHK 要写脚本。

另一个不那么显眼但真实的理由：键盘钩子在技术上等同于键盘记录器。本项目全部实现约 1200 行，可以逐行读完，而 AutoHotkey 是几十万行 C++。在公司电脑或安全敏感环境里，这有时是决定性的。

## 安装

### scoop

```
scoop bucket add huanfeng https://github.com/huanfeng/scoop-bucket
scoop install keyremap
```

装完直接运行 `keyremap` 即可。配置会自动生成在 `%APPDATA%\keyremap\keyremap.toml`，升级不会动它。

### 绿色版

1. 下载 release 里的压缩包，解压到任意目录（`keyremap.exe` 与 `keyremap.toml` 需在同一目录）
2. 按需编辑 `keyremap.toml`
3. 双击运行 —— 没有控制台窗口，托盘出现图标即已生效

## 快速开始

不确定某个键叫什么名字，用**托盘菜单 → 按键监听**打开一个监听窗口，按下它看输出。命令行等价形式：

```
keyremap.exe --listen
```

开机自启：**托盘菜单 → 开机自启动**，勾上即可。

同一个 exe 只能运行一个实例，但把整个目录复制一份到别处，就能带着各自的配置同时跑多份。

## 配置

### 配置文件在哪

按下面的顺序查找，用第一个命中的：

| 顺序 | 位置 | 用于 |
|---|---|---|
| 1 | `-c <路径>` 指定的文件 | 手动指定 |
| 2 | exe 同目录的 `keyremap.toml` | **绿色版**，整个目录拷走就带走配置 |
| 3 | `%APPDATA%\keyremap\keyremap.toml` | **安装版**，首次运行自动生成 |

不确定当前用的是哪个，跑 `keyremap --dump` 看第一行。

scoop 安装时第 2 条会被**跳过**：压缩包里自带的 `keyremap.toml` 会被解压进 `apps\keyremap\<版本>\`，而那个目录每次升级都会被换掉。把配置放在那里，用户改完下次升级就静默丢了。所以 scoop 装的一律走第 3 条，安装目录里那份只作示例（程序会在日志里说明这一点）。

同理，开机自启记下的程序路径走 `apps\keyremap\current\`（scoop 指向当前版本的联接）而不是带版本号的真实路径，否则升级之后自启会指向一个已被删除的目录。

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

### 组合键作为输入源

`from` 也能写成数组：**最后一个是触发键，前面的都是要按住的修饰键**。

```toml
[[mappings]]
name = "Ctrl+E转Backspace"
from = ["Ctrl", "E"]
to = "Backspace"
```

修饰键写 `Ctrl` / `Shift` / `Alt` / `Win` 表示**左右任一**按下都算，这是常见需求。要限定某一侧就写 `LCtrl` / `RCtrl` / `LShift` / `RShift` / `LAlt` / `RAlt` / `LWin` / `RWin`：

```toml
# 只有右 Alt 才触发，左 Alt 不受影响
[[mappings]]
from = ["RAlt", "J"]
to = "Down"
```

触发键可以是鼠标按键：

```toml
[[mappings]]
name = "恢复关闭的标签页"
from = ["Ctrl", "MouseX2"]
to = ["Ctrl", "Shift", "T"]
```

几条需要知道的规则：

- **触发时会先松开修饰键，发完再按回去。** 你按下 `Ctrl` 的那一下已经放行给应用了，此时直接注入 `Backspace`，应用收到的是 `Ctrl+Backspace`（多数编辑器会删掉一整个词）。整批放在一次 `SendInput` 里发出，应用不会看到中间状态，但**确实会看到一次 Ctrl 的松开与重新按下** —— 这是这个方案避不开的代价。只有 `from` 里点名的修饰键会被松开，你同时按着的其他键不动；松开和按回的都是**你当时实际按着的那一侧**，按右 Ctrl 就动右 Ctrl。
- **长按连发跟着 `to` 走**：`to` 是单键就连发（长按 `Ctrl+E` 连续删字），是组合键就只触发一次（否则 `Ctrl+W` 一按住会连关一串标签页）。
- **更具体的匹配优先**：同时配了 `E → D` 和 `Ctrl+E → Backspace`，按住 Ctrl 走后者。否则就成了看谁写在配置文件前面，没法解释。
- 触发键本身不能是修饰键。`["Ctrl", "Shift"]` 到底是"按住 Ctrl 再按 Shift"还是"同时按住两个"没有明确含义，直接报错而不去猜。

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

用 `keyremap.exe --dump-keys` 可以输出完整的 JSON 列表。键名不区分大小写。

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

## Web 配置工具

**托盘菜单 → Web 配置工具**，打开一个网页版配置界面：按键捕获、组合键勾选、分组管理、冲突提示、TOML 预览。

- Chrome / Edge：可以直接打开并保存 `keyremap.toml`，文件授权会被记住
- 其他浏览器：编辑后复制 TOML 粘贴进配置文件

两种方式保存后主程序都会自动重载。键名表由 `--dump-keys` 导出，与主程序同源，CI 会校验二者一致。

**主程序本身不含任何网络代码** —— 它只是把地址交给系统浏览器去开，既不发请求也不解析响应。

页面默认指向仓库自带的 GitHub Pages 部署。想换成自己部署的（比如更快的 Cloudflare Pages），在配置里写：

```toml
web_url = "https://my-keyremap.pages.dev/"
```

页面代码在 `docs/` 目录（GitHub Pages 从这个目录零配置部署），详见 [docs/README.md](docs/README.md)。

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
    Web 配置工具...             用系统浏览器打开网页版配置界面
─────────────
[✓] 开机自启动
[✓] 以管理员身份启动            持久设置，手动启动与开机自启都生效
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

### 以管理员身份启动

Windows 的 UIPI 机制禁止低权限进程向高权限进程注入输入 —— 所以当前台窗口属于一个管理员权限的程序时（任务管理器、提权运行的编辑器等），普通权限的 keyremap 发出的按键会被丢弃，表现为"在某些窗口里映射突然不生效"。

勾上**托盘菜单 → 以管理员身份启动**即可解决。这是个**持久设置**，不是一次性动作：勾上之后手动启动、开机自启都会以管理员身份运行，不用每次重新设置。

它是怎么做到的：

| 组合 | 实际机制 |
|---|---|
| 自启 + 普通权限 | `HKCU\...\Run` 项 |
| 自启 + 管理员 | 任务计划程序里一个 `/RL HIGHEST` 的登录任务 |

之所以要换机制：**`HKCU\Run` 启动的进程永远不会提权**，这是 Windows 的设计。只有计划任务能做到"开机自启且以管理员运行、且不弹 UAC"。

首次勾选时会请求一次 UAC 确认并重启程序 —— 因为创建 `/RL HIGHEST` 任务本身就需要管理员权限。之后每次启动都不会再打扰你。UAC 被取消时程序会以普通权限继续运行（而不是干脆不启动），并在日志里说明。

托盘提示里的 `· 管理员` 表示**此刻真的以管理员在跑**；菜单里的勾选表示**设置**。刚改完设置还没重启时两者会不一致，这是正常的。

### 托盘图标的状态

图标会跟着两件事变：

- **启用 / 禁用** —— 禁用时图标变淡
- **任务栏主题** —— 深色任务栏用浅色图标，浅色任务栏用深色图标

后一条是必需的：图标是单色线条，纯黑的放在深色任务栏上几乎看不见，而 Windows 11 默认就是深色任务栏。系统主题切换时图标会自动跟着换，不用重启。

## 命令行参数

程序是 GUI 子系统的，双击不会有黑窗口；需要输出时会自动附加控制台 —— 从终端运行就附到当前终端，双击运行则新开一个窗口。

| 参数 | 说明 |
|---|---|
| `-c, --config <PATH>` | 指定配置文件，默认见[配置文件在哪](#配置文件在哪) |
| `-l, --listen` | 监听模式，只打印按下的键名，不做任何映射（窗口会关掉快速编辑，见下） |
| `--dump` | 按组打印已加载的映射后退出 |
| `--dump-keys` | 输出全部可用键名（JSON）后退出 |
| `-o, --output <FILE>` | 配合上面两个：把输出写入文件 |
| `--logfile` | 把日志写入 exe 同目录的 `keyremap.log` |
| `-v`, `-vv` | 提高日志级别，并自动附加控制台显示日志 |

**要落盘请用 `-o` 而不是 shell 重定向**：GUI 程序不会被 shell 等待，`--dump-keys > keys.json` 会因为管道提前关闭而写不进去。

一次性输出的命令（`--dump` / `--dump-keys` / `--version` / `--help`）如果是自己新开的窗口，会等你按回车再关，不会闪一下就没了。什么时候会新开窗口：双击运行，或者经 scoop 的 shim 启动 —— 那个 shim 不等子进程就退出，附不到调用方的终端上。输出重定向到文件时不会等待，脚本里可以放心用。

**监听窗口关掉了"快速编辑模式"**。开着的话在窗口里点一下鼠标就进入选区，而选区会让控制台写入一直阻塞到选区取消 —— 对本程序来说这意味着按键事件的输出通道被堵住。想从监听窗口复制文字，用标题栏右键菜单里的**编辑 → 标记**。

同一个 exe 只允许运行一个实例 —— 多个实例各装一套钩子会互相干扰，出现"已禁用却仍在生效"这类现象。锁的粒度是**程序路径**，所以复制到别处的另一份可以独立运行。`--dump` 和 `--listen` 不受此限制，方便在后台运行时排查。

## 已知限制

- **UAC 安全桌面与部分反作弊游戏覆盖不到**。这类场景任何用户态方案都无能为力，需要内核级驱动。普通的管理员权限窗口用[以管理员身份启动](#以管理员身份启动)即可解决。
- **组合键作为输入源时，应用会看到一次修饰键的松开与重新按下**。原因见[组合键作为输入源](#组合键作为输入源)，用户态方案绕不开。
- **Web 配置工具只给四个通用修饰键的勾选框**。分侧的（`LCtrl` 等）需要手写配置；不过已经写在配置里的会被认出来并显示成勾选框，不会被工具改掉。
- **`CapsLock` 还不能当修饰键用**。那需要把 CapsLock 自身的大小写切换也吞掉，属于"层"的范畴，暂未实现。
- `to` 暂不支持鼠标按键输出。
- 仅支持 Windows。实现直接调用 Win32 API，没有跨平台打算。

## 构建

```
cargo build --release
```

需要 Rust 1.85+（edition 2024）。产物在 `target/release/keyremap.exe`。

**不需要资源编译器** —— `assets/app.res`（四个图标变体 + 文件版本信息）是预编译好提交进仓库的，克隆下来直接 `cargo build` 即可。只有改动图标或版本号时才需要重新生成：

```powershell
python assets\make-icons.py    # 图标源变了：从 app_icon.ico 派生其余三个变体
.\assets\build-res.ps1         # 重新编译 app.res（用 rc.exe 或 windres）
```

改了 `Cargo.toml` 的版本号，要同步改 `assets/app.rc` 里的三处版本字段再重新生成。CI 会比对 exe 的版本资源与 `Cargo.toml`，忘记时构建会失败。

## 发布

版本号有三处，发版前必须先在本地对齐：

1. `Cargo.toml` 的 `version`
2. `assets/app.rc` 的 `FILEVERSION` / `FileVersion` / `ProductVersion`
3. 跑 `.\assets\build-res.ps1` 重新生成 `assets/app.res`，提交

然后打 tag 推送：

```
git tag v0.2.0 && git push origin v0.2.0
```

`release.yml` 会校验 tag、`Cargo.toml`、exe 版本资源三者一致——不一致就直接失败，而不是发一个自相矛盾的包（流水线改不了预编译的 `app.res`，所以只能校验不能自动修）。

通过之后自动：构建 → 打包 `keyremap-<版本>-windows-x86_64.zip` → 建 Release → 向 [huanfeng/scoop-bucket](https://github.com/huanfeng/scoop-bucket) 发 `repository_dispatch`，由对端下载资产、算 sha256、更新清单。

压缩包内**不套目录**，文件都在根。scoop 直接解压到应用目录，多一层壳就得靠清单里的 `extract_dir`，而那个值带版本号，自动更新的清单维护不了。

需要在本仓库配置 secret `SCOOP_BUCKET_GITHUB_TOKEN`（对 scoop-bucket 仓库有 `contents:write` 的 PAT）。

## 实现说明

如果你打算读代码或者做类似的东西，几个可能有用的点：

- 键盘和鼠标用 `SetWindowsHookExW` 装 `WH_KEYBOARD_LL` / `WH_MOUSE_LL` 低级钩子。这是用户态下唯一能**拦截**并改写按键的方式，AutoHotkey 和 PowerToys 也都是这套。`RegisterHotKey` 不能改键，Raw Input 只能观察不能拦截。
- 注入事件的 `dwExtraInfo` 打上标记，钩子回调第一件事就是检查它并放行自己发出的事件。没有这一步，反向映射会无限回环。AutoHotkey 用的是同一手法。
- 钩子回调里不做任何耗时操作：不 sleep、不做字符串比较、不调 `ToUnicodeEx`。低级钩子回调超过 `LowLevelHooksTimeout`（默认 300 ms）会被系统**静默摘掉**，程序不崩但从此失效。组合键因此用单次 `SendInput` 原子发送，而不是逐个发再 sleep。
- 配置在加载时就"编译"成虚拟键码，热路径上只比较整数。
- 键盘钩子、托盘窗口消息、配置重载通知共用同一个消息循环，不额外开线程 —— 低级钩子本来就需要消息泵，托盘图标也需要窗口，两者天然可以合并。
- 配置里没有鼠标映射时不安装鼠标钩子。`WH_MOUSE_LL` 会收到全部鼠标移动事件，鼠标一动就进一次回调，这是常态下最大的一笔无谓开销。**这个判断每次配置重载都会重做一遍**：配置里新出现鼠标映射就补装、没有了就卸掉。托盘提示里的「含鼠标」反映的是钩子的实际状态。
- 钩子回调里不做任何 IO。监听模式的打印是投给另一个线程做的 —— 控制台被鼠标选中时写入会阻塞，而回调阻塞意味着**全系统的输入**都在等它，卡住的不是本程序而是整台机器。队列满了就丢事件并计数，宁可少打一行日志。

## License

MIT
