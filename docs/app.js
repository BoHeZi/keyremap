'use strict';

// keyremap 配置工具。纯静态、无构建步骤，只依赖同目录下的 Alpine.js。
//
// 两种工作方式:
//   整文件模式 —— File System Access API 直接读写 keyremap.toml，
//                 文件句柄存进 IndexedDB，下次打开点一次授权就能续上。
//   片段模式   —— 不碰文件，编辑完复制 TOML 粘贴进配置文件。
//                 Firefox / Safari 没有 FSA API，会自动落到这条路。
//
// 保存后主程序的文件监听会自动重载，两种方式的反馈都是即时的。

// ---------- 常量 ----------

/** 作为"修饰键"呈现在勾选框里的键。数组顺序即注入顺序。 */
const MODIFIERS = ['Ctrl', 'Alt', 'Shift', 'LWin'];

/**
 * 浏览器 KeyboardEvent.code -> 本项目键名。
 *
 * 这是唯一一份必须手写的对照表 —— 主程序不可能知道浏览器的命名习惯。
 * 转换结果会用 keys.js 里的表校验一遍，对不上就当捕获失败，
 * 绝不产出主程序不认识的键名。
 */
const CODE_MAP = (() => {
  const map = {
    Escape: 'Esc', Enter: 'Enter', Backspace: 'Backspace', Tab: 'Tab',
    Space: 'Space', CapsLock: 'CapsLock',
    ControlLeft: 'LCtrl', ControlRight: 'RCtrl',
    AltLeft: 'LAlt', AltRight: 'RAlt',
    ShiftLeft: 'LShift', ShiftRight: 'RShift',
    MetaLeft: 'LWin', MetaRight: 'RWin', ContextMenu: 'Apps',
    Insert: 'Insert', Delete: 'Delete', Home: 'Home', End: 'End',
    PageUp: 'PageUp', PageDown: 'PageDown',
    ArrowLeft: 'Left', ArrowRight: 'Right', ArrowUp: 'Up', ArrowDown: 'Down',
    PrintScreen: 'PrintScreen', ScrollLock: 'ScrollLock', Pause: 'Pause',
    NumLock: 'NumLock',
    Backquote: 'Backquote', Minus: 'Minus', Equal: 'Equal',
    BracketLeft: 'LeftBracket', BracketRight: 'RightBracket',
    Backslash: 'Backslash', Semicolon: 'Semicolon', Quote: 'Quote',
    Comma: 'Comma', Period: 'Period', Slash: 'Slash',
    NumpadAdd: 'NumpadAdd', NumpadSubtract: 'NumpadSubtract',
    NumpadMultiply: 'NumpadMultiply', NumpadDivide: 'NumpadDivide',
    NumpadDecimal: 'NumpadDecimal', NumpadEnter: 'Enter',
    AudioVolumeMute: 'VolumeMute', AudioVolumeDown: 'VolumeDown',
    AudioVolumeUp: 'VolumeUp', MediaTrackNext: 'MediaNext',
    MediaTrackPrevious: 'MediaPrev', MediaStop: 'MediaStop',
    MediaPlayPause: 'MediaPlayPause',
  };
  for (let c = 65; c <= 90; c++) map['Key' + String.fromCharCode(c)] = String.fromCharCode(c);
  for (let d = 0; d <= 9; d++) { map['Digit' + d] = String(d); map['Numpad' + d] = 'Numpad' + d; }
  for (let f = 1; f <= 24; f++) map['F' + f] = 'F' + f;
  return map;
})();

const DEFAULT_HEADER =
  '# keyremap 配置\n# 用 keyremap --dump-keys 可以列出全部可用键名';

// ---------- TOML 解析 ----------

function unquote(s) {
  const t = s.trim();
  if ((t.startsWith('"') && t.endsWith('"')) || (t.startsWith("'") && t.endsWith("'"))) {
    return t.slice(1, -1).replace(/\\"/g, '"').replace(/\\\\/g, '\\');
  }
  return t;
}

/** 去掉不在引号内的行尾注释 */
function stripComment(s) {
  let inStr = false;
  for (let i = 0; i < s.length; i++) {
    if (s[i] === '"' && s[i - 1] !== '\\') inStr = !inStr;
    else if (s[i] === '#' && !inStr) return s.slice(0, i);
  }
  return s;
}

function parseValue(raw) {
  const v = raw.trim();
  if (v === 'true') return true;
  if (v === 'false') return false;
  if (v.startsWith('[')) {
    return v.slice(1, v.lastIndexOf(']')).split(',')
      .map(unquote).filter((x) => x !== '');
  }
  return unquote(v);
}

/**
 * 解析配置。只覆盖本项目用到的 TOML 子集，够用且可控。
 * 无法识别的内容进 warnings，不会静默丢弃。
 */
function parseToml(text) {
  const lines = text.split(/\r?\n/);
  const out = { header: '', name: '', webUrl: '', groupEnabled: {}, mappings: [], warnings: [] };
  const headerLines = [];
  let inHeader = true;
  let section = null; // 'groups' | 'mapping'
  let current = null;

  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    const line = stripComment(raw).trim();

    if (line.startsWith('[')) {
      inHeader = false;
      if (line === '[[mappings]]') {
        current = newMapping();
        out.mappings.push(current);
        section = 'mapping';
      } else if (line === '[groups]') {
        section = 'groups';
        current = null;
      } else {
        out.warnings.push(`第 ${i + 1} 行: 无法识别的段 ${line}`);
        section = null;
      }
      continue;
    }

    // 首个段之前的注释与空行原样保留，键值行照常解析
    if (inHeader && (line === '' || raw.trimStart().startsWith('#'))) {
      headerLines.push(raw);
      continue;
    }
    if (line === '') continue;

    const eq = line.indexOf('=');
    if (eq < 0) {
      out.warnings.push(`第 ${i + 1} 行: 看不懂，已忽略 —— ${raw.trim()}`);
      continue;
    }

    const key = unquote(line.slice(0, eq));
    let valText = line.slice(eq + 1).trim();

    // 跨行数组: 读到右括号为止
    if (valText.startsWith('[') && !valText.includes(']')) {
      while (++i < lines.length) {
        valText += ' ' + stripComment(lines[i]).trim();
        if (valText.includes(']')) break;
      }
    }
    const value = parseValue(valText);

    if (section === 'mapping' && current) {
      if (key === 'to') current.to = Array.isArray(value) ? value : [value];
      else if (key in current) current[key] = value;
      else out.warnings.push(`第 ${i + 1} 行: 映射里无法识别的字段 ${key}`);
    } else if (section === 'groups') {
      if (value === false) out.groupEnabled[key] = false;
    } else if (key === 'name') {
      out.name = String(value);
    } else if (key === 'web_url') {
      // 必须认识这个字段, 否则用本工具保存一次就把它吃掉了
      out.webUrl = String(value);
    } else {
      out.warnings.push(`第 ${i + 1} 行: 位置不明的字段 ${key}`);
    }
  }

  // 去掉首尾空行: 原文件里 name = "..." 之后的空行会被收进 headerLines,
  // 不清掉的话每次生成都会在开头多出一个空行
  out.header = headerLines.join('\n').replace(/^\s*\n/, '').replace(/\s+$/, '');
  return out;
}

// ---------- TOML 生成 ----------

const escStr = (s) => String(s).replace(/\\/g, '\\\\').replace(/"/g, '\\"');

let nextId = 1;
function newMapping() {
  // _id 只用于列表渲染的 key，不写进 TOML
  return { _id: nextId++, name: '', comment: '', enable: true, group: '', from: '', to: [] };
}

// ---------- 文件句柄的持久化 ----------

const IDB_NAME = 'keyremap';
const IDB_STORE = 'handles';

function openIdb() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(IDB_NAME, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(IDB_STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

async function idbSet(key, value) {
  const db = await openIdb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(IDB_STORE, 'readwrite');
    tx.objectStore(IDB_STORE).put(value, key);
    tx.oncomplete = resolve;
    tx.onerror = () => reject(tx.error);
  });
}

async function idbGet(key) {
  const db = await openIdb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(IDB_STORE, 'readonly');
    const req = tx.objectStore(IDB_STORE).get(key);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

// ---------- Alpine 组件 ----------

function configApp() {
  return {
    MODIFIERS,
    KEYS: window.KEYREMAP_KEYS || { keys: [], mouse: [] },
    hasFSA: typeof window.showOpenFilePicker === 'function',

    header: DEFAULT_HEADER,
    name: '',
    /** 配置里的 web_url。本工具不编辑它，但必须原样带回去，否则保存会丢字段 */
    webUrl: '',
    /** { 组名: false } —— 只记录被关掉的组，未列出即启用 */
    groupEnabled: {},
    mappings: [],

    fileHandle: null,
    /** 上次编辑过的文件名，非空时首个按钮变成"继续编辑 xxx" */
    restoreName: '',
    mode: '未加载',
    note: '',
    noteError: false,

    capturing: false,
    captureTarget: null,

    init() {
      if (!this.KEYS.keys.length) {
        this.say('键名表载入失败，下拉列表会是空的', true);
      }
      this.tryRestore();
    },

    // ---- 派生数据 ----

    /** 用到的组名，按映射中首次出现的顺序 */
    get groups() {
      const seen = [];
      for (const m of this.mappings) {
        if (m.group && !seen.includes(m.group)) seen.push(m.group);
      }
      return seen;
    },

    /** 与主程序同样的规则: 只有同时生效的重复输入源才算冲突 */
    get conflicts() {
      const seen = new Map();
      const out = [];
      for (const m of this.mappings) {
        if (!this.isActive(m) || !m.from) continue;
        if (seen.has(m.from)) out.push([seen.get(m.from), m]);
        else seen.set(m.from, m);
      }
      return out;
    },

    get toml() {
      let out = '';
      if (this.header) out += this.header + '\n\n';
      if (this.name) out += `name = "${escStr(this.name)}"\n`;
      if (this.webUrl) out += `web_url = "${escStr(this.webUrl)}"\n`;

      const off = this.groups.filter((g) => this.groupEnabled[g] === false);
      if (off.length) {
        out += '\n[groups]\n';
        for (const g of off) out += `"${escStr(g)}" = false\n`;
      }

      for (const m of this.mappings) {
        out += '\n[[mappings]]\n';
        if (m.name) out += `name = "${escStr(m.name)}"\n`;
        if (m.comment) out += `comment = "${escStr(m.comment)}"\n`;
        if (m.enable === false) out += 'enable = false\n';
        if (m.group) out += `group = "${escStr(m.group)}"\n`;
        out += `from = "${escStr(m.from)}"\n`;
        out += m.to.length === 1
          ? `to = "${escStr(m.to[0])}"\n`
          : `to = [${m.to.map((k) => `"${escStr(k)}"`).join(', ')}]\n`;
      }
      return out;
    },

    isActive(m) {
      return m.enable !== false && this.groupEnabled[m.group] !== false;
    },

    isConflicted(m) {
      return this.conflicts.some(([a, b]) => a === m || b === m);
    },

    countInGroup(g) {
      return this.mappings.filter((m) => m.group === g).length;
    },

    // ---- 输出 (to) 的编辑 ----

    /** to 能否表示成"若干修饰键 + 一个主键" */
    isSimpleTo(m) {
      return m.to.filter((k) => !MODIFIERS.includes(k)).length === 1;
    },
    hasMod(m, mod) {
      return m.to.includes(mod);
    },
    mainKey(m) {
      return m.to.find((k) => !MODIFIERS.includes(k)) || '';
    },
    /** 每次都从当前 to 重算，不依赖任何渲染时的快照 */
    setMod(m, mod, on) {
      const mods = MODIFIERS.filter((x) => (x === mod ? on : m.to.includes(x)));
      m.to = [...mods, this.mainKey(m)].filter(Boolean);
    },
    setMainKey(m, key) {
      const mods = MODIFIERS.filter((x) => m.to.includes(x));
      m.to = [...mods, key].filter(Boolean);
    },

    // ---- 条目增删 ----

    addMapping() {
      const first = this.KEYS.keys[0] || '';
      const m = newMapping();
      m.from = first;
      m.to = [first];
      this.mappings.push(m);
    },

    removeMapping(m) {
      const i = this.mappings.indexOf(m);
      if (i >= 0) this.mappings.splice(i, 1);
    },

    setGroupEnabled(g, on) {
      if (on) delete this.groupEnabled[g];
      else this.groupEnabled[g] = false;
    },

    newConfig() {
      this.header = DEFAULT_HEADER;
      this.name = '我的配置';
      this.webUrl = '';
      this.groupEnabled = {};
      this.mappings = [];
      this.fileHandle = null;
      this.restoreName = '';
      this.mode = '新配置（未关联文件）';
    },

    // ---- 按键捕获 ----

    startCapture(m) {
      this.captureTarget = m;
      this.capturing = true;
    },

    finishCapture(key) {
      if (key && this.captureTarget) this.captureTarget.from = key;
      this.capturing = false;
      this.captureTarget = null;
    },

    onCaptureKey(e) {
      if (!this.capturing) return;
      if (e.code === 'Escape') return this.finishCapture(null);

      const name = CODE_MAP[e.code];
      const known = [...this.KEYS.keys, ...this.KEYS.mouse];
      if (name && known.includes(name)) this.finishCapture(name);
      else this.say(`这个键暂不支持捕获 (code=${e.code})，请从下拉里选`, true);
    },

    // ---- 文件读写 ----

    applyParsed(p) {
      this.header = p.header;
      this.name = p.name;
      this.webUrl = p.webUrl;
      this.groupEnabled = p.groupEnabled;
      this.mappings = p.mappings;
      if (p.warnings.length) {
        this.say(`有 ${p.warnings.length} 处内容没能识别: ${p.warnings[0]}`, true);
      }
    },

    async loadFromHandle(handle) {
      const text = await (await handle.getFile()).text();
      this.applyParsed(parseToml(text));
      this.fileHandle = handle;
      this.restoreName = '';
      this.mode = `已打开 ${handle.name}`;
    },

    async openFile() {
      // 有上次留下的句柄就先试着续上, 省掉再选一次文件
      if (this._pending) {
        const handle = this._pending;
        this._pending = null;
        this.restoreName = '';
        try {
          if ((await handle.requestPermission({ mode: 'readwrite' })) === 'granted') {
            await this.loadFromHandle(handle);
            return;
          }
        } catch {
          /* 授权失败就走正常的选文件流程 */
        }
      }

      if (!this.hasFSA) {
        // 没有 FSA 的浏览器退回普通文件选择: 能读，但存不回去
        const input = document.createElement('input');
        input.type = 'file';
        input.accept = '.toml';
        input.onchange = async () => {
          const f = input.files[0];
          if (!f) return;
          this.applyParsed(parseToml(await f.text()));
          this.mode = `已载入 ${f.name}（只读）`;
        };
        input.click();
        return;
      }

      try {
        const [handle] = await window.showOpenFilePicker({
          types: [{ description: 'TOML 配置', accept: { 'text/plain': ['.toml'] } }],
        });
        await idbSet('config', handle);
        await this.loadFromHandle(handle);
      } catch (e) {
        if (e.name !== 'AbortError') this.say(`打开失败: ${e.message}`, true);
      }
    },

    async saveFile() {
      if (!this.fileHandle) return;
      try {
        if ((await this.fileHandle.requestPermission({ mode: 'readwrite' })) !== 'granted') {
          this.say('没有写入权限', true);
          return;
        }
        const w = await this.fileHandle.createWritable();
        await w.write(this.toml);
        await w.close();
        this.say('已保存 —— 主程序会自动重载');
      } catch (e) {
        this.say(`保存失败: ${e.message}`, true);
      }
    },

    /** 页面打开时尝试续上上次编辑的文件 */
    async tryRestore() {
      if (!this.hasFSA) return;
      try {
        const handle = await idbGet('config');
        if (!handle) return;
        if ((await handle.queryPermission({ mode: 'readwrite' })) === 'granted') {
          await this.loadFromHandle(handle);
        } else {
          // 恢复权限必须由用户手势触发，先把按钮改成"继续编辑 xxx"
          this.restoreName = handle.name;
          this.mode = `上次编辑过 ${handle.name}`;
          this._pending = handle;
        }
      } catch {
        /* 句柄失效就当没有 */
      }
    },

    async copyToml() {
      try {
        await navigator.clipboard.writeText(this.toml);
        this.say('已复制 —— 粘贴进 keyremap.toml 保存即可生效');
      } catch {
        this.say('复制失败，请手动选中下方内容复制', true);
      }
    },

    downloadToml() {
      const blob = new Blob([this.toml], { type: 'text/plain' });
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = 'keyremap.toml';
      a.click();
      URL.revokeObjectURL(a.href);
    },

    say(msg, isError = false) {
      this.note = msg;
      this.noteError = isError;
      clearTimeout(this._noteTimer);
      this._noteTimer = setTimeout(() => { this.note = ''; }, 6000);
    },
  };
}
