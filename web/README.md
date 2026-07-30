# keyremap-ng 配置工具（Web）

纯静态页面，无构建步骤。可视化编辑 `keyremap.toml`：按键捕获、组合键搭配、分组管理、冲突提示。

## 两种工作方式

**整文件模式** —— 直接打开并保存 `keyremap.toml`。
依赖 File System Access API（Chrome / Edge 支持），文件句柄存进 IndexedDB，
下次打开页面点一次授权就能续上，不用每次重新选文件。

**片段模式** —— 不碰文件，编辑完复制 TOML 粘贴进配置文件。
Firefox / Safari 没有 FSA API，会自动落到这条路。

两种方式保存后主程序都会自动重载并弹气泡，不需要重启。

## 部署

任意静态托管即可（Cloudflare Pages / GitHub Pages），把本目录作为站点根目录。

**必须是 HTTPS 或 localhost** —— File System Access API 要求安全上下文，
`file://` 下该 API 直接不存在（页面仍可用，但只有片段模式）。

无需构建命令，也不需要 Node 环境。

## 更新键名表

`keys.js` 是从主程序导出的，**不要手改**。主程序的键名表变动后重新生成：

```bash
keyremap-ng --dump-keys -o web/keys.json
# 包装成 script 可加载的形式
{ printf 'window.KEYREMAP_KEYS = '; cat web/keys.json; printf ';\n'; } > web/keys.js
```

PowerShell：

```powershell
.\keyremap-ng.exe --dump-keys -o web\keys.json
"window.KEYREMAP_KEYS = " + (Get-Content web\keys.json -Raw) + ";" | Set-Content web\keys.js
```

CI 会校验 `keys.js` 与 `--dump-keys` 的输出是否一致，不一致就构建失败。

用 `<script>` 而不是 `fetch('keys.json')` 加载，是为了让页面在 `file://` 下也能工作 ——
`fetch` 本地文件会被 CORS 挡住。`keys.json` 保留下来供 CI 校验和其他用途。

## 已知的边界

- **注释**：文件开头（第一个 `[...]` 段之前）的注释会原样保留；散落在映射之间的裸
  `#` 注释在保存时会丢失。每条映射的说明请写进 `comment = "..."` 字段 ——
  它是结构化的，页面能直接编辑，也永远不会丢。
- **按键捕获**：`Ctrl+W`、`Ctrl+T`、`Ctrl+N` 等是浏览器保留快捷键，网页收得到事件
  但阻止不了浏览器的默认行为（页面会被关掉）。所以组合键请用**修饰键勾选框**搭，
  不要整个按下去。捕获功能主要用于单键与鼠标侧键。
- **TOML 解析**：只覆盖本项目实际用到的子集（顶层 `name`、`[groups]` 表、
  `[[mappings]]` 数组表、字符串/布尔/单行与跨行数组）。解析不了的内容会提示出来，
  不会静默丢弃。

## 文件

| 文件 | 说明 |
|---|---|
| `index.html` | 页面结构，Alpine 声明式模板 |
| `app.js` | TOML 解析与生成、文件读写、按键捕获、状态 |
| `style.css` | 样式，跟随系统深浅色 |
| `keys.js` | 键名表，由主程序导出 |
| `keys.json` | 同上的 JSON 形式，供 CI 校验 |
| `vendor/alpine.min.js` | Alpine.js，**故意放进仓库而不引 CDN** —— 这样能离线使用、不依赖第三方可用性、也不把访问记录暴露给 CDN |
