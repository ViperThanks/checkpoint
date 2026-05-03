// view_model.js — 共享视图模型层：纯函数，无 DOM 依赖
//
// 职责：HTML 转义、JS 字符串安全、ID 截断、时间格式化、Agent 标签映射、
// toast 通知、agent 日志清洗。
//
// 来源：bridge app.js + relay app.js 去重合并。
// 两端 shell 不再各自定义这些函数，统一引用此模块。
//
// 环境兼容：
// - 浏览器：被 ui.rs / mobile_ui.rs include_str! 后注入 <script>，函数挂全局
// - Node.js：通过 module.exports 导出，供测试直接 require

// ============================================================
// HTML 转义
// ============================================================

/**
 * 将字符串中的 HTML 特殊字符转义为实体。
 * 用于所有动态内容插入 DOM 时的 XSS 防护。
 *
 * @param {*} s - 任意值，内部转为 String
 * @returns {string} 转义后的安全 HTML 字符串
 */
function escHtml(s) {
  if (s == null) return '';
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

// bridge 兼容别名（bridge 原先用 esc()）
var esc = escHtml;

// ============================================================
// JS 字符串安全
// ============================================================

/**
 * 转义字符串以安全嵌入 JavaScript 单引号字符串上下文。
 * 同时转义反斜杠、单引号和双引号。
 *
 * @param {*} s
 * @returns {string}
 */
function jsStr(s) {
  if (s == null) return '';
  return String(s).replace(/\\/g, '\\\\').replace(/'/g, "\\'").replace(/"/g, '\\"').replace(/\n/g, '\\n').replace(/\r/g, '\\r');
}

// ============================================================
// ID / 文本截断
// ============================================================

/**
 * 将长 ID 压缩成可读的短标签。
 * 不变量：只影响展示，不改变任何 API 使用的真实 ID。
 *
 * @param {string} id
 * @returns {string}
 */
function shortId(id) {
  if (!id) return '';
  return id.length > 8 ? id.slice(0, 8) + '...' : id;
}

/**
 * 截断字符串到指定长度，超出部分用 '...' 替代。
 *
 * @param {string} s
 * @param {number} n - 最大长度
 * @returns {string}
 */
function trunc(s, n) {
  if (!s) return '';
  return s.length > n ? s.substring(0, n) + '...' : s;
}

// ============================================================
// 时间格式化
// ============================================================

/**
 * 格式化时间戳为绝对时间字符串。
 * 当天显示 HH:MM，跨月显示 MM/DD HH:MM。
 *
 * @param {string} ts - ISO 时间戳
 * @returns {string}
 */
function formatTime(ts) {
  if (!ts) return '';
  try {
    var d = new Date(ts);
    if (isNaN(d.getTime())) return '';
    var now = new Date();
    var sameDay = d.toDateString() === now.toDateString();
    var h = d.getHours().toString().padStart(2, '0');
    var min = d.getMinutes().toString().padStart(2, '0');
    if (sameDay) return h + ':' + min;
    var mo = (d.getMonth() + 1).toString().padStart(2, '0');
    var day = d.getDate().toString().padStart(2, '0');
    return mo + '/' + day + ' ' + h + ':' + min;
  } catch (e) { return ''; }
}

// bridge 兼容别名
var formatMsgTime = formatTime;

/**
 * 格式化时间戳为相对时间字符串（中文）。
 *
 * @param {string} ts - ISO 时间戳
 * @returns {string}
 */
function relTime(ts) {
  if (!ts) return '';
  var d = new Date(ts);
  if (isNaN(d)) return ts;
  var now = Date.now();
  var diff = now - d.getTime();
  if (diff < 60000) return '刚刚';
  if (diff < 3600000) return Math.floor(diff / 60000) + ' 分钟前';
  if (diff < 86400000) return Math.floor(diff / 3600000) + ' 小时前';
  if (diff < 604800000) return Math.floor(diff / 86400000) + ' 天前';
  return d.toLocaleDateString('zh-CN');
}

// bridge 兼容别名
var ago = relTime;

// ============================================================
// Agent 标签
// ============================================================

/**
 * 将 agent 标识符转为人类可读的显示名。
 *
 * @param {string} agent
 * @returns {string}
 */
function agentLabel(agent) {
  if (!agent) return '未知';
  var map = {
    'claude_code': 'Claude Code', 'claude': 'Claude Code',
    'codex_cli': 'Codex CLI', 'codex': 'Codex CLI',
    'kimi_code': 'Kimi Code', 'kimi': 'Kimi Code',
    'gemini_cli': 'Gemini CLI', 'gemini': 'Gemini CLI',
  };
  return map[agent] || agent;
}

// ============================================================
// 项目路径
// ============================================================

/**
 * 提取项目路径的最后两级目录作为短标签。
 *
 * @param {string} path
 * @returns {string}
 */
function shortProject(path) {
  if (!path) return '';
  var parts = path.split('/');
  return parts.length > 2 ? parts.slice(-2).join('/') : path;
}

/**
 * 提取项目路径的 basename。
 *
 * @param {string} p
 * @returns {string}
 */
function projectBasename(p) {
  if (!p) return '';
  var parts = p.replace(/\/$/, '').split('/');
  return parts[parts.length - 1] || p;
}

// ============================================================
// Toast 通知
// ============================================================

/**
 * 显示短暂的 toast 消息。
 * 需要 DOM 中存在 id="toast-container" 的容器。
 *
 * @param {string} msg
 */
function toast(msg) {
  var c = document.getElementById('toast-container');
  if (!c) {
    // relay 没有 toast-container 时动态创建
    c = document.createElement('div');
    c.id = 'toast-container';
    c.style.cssText = 'position:fixed;bottom:20px;left:50%;transform:translateX(-50%);z-index:300;display:flex;flex-direction:column;gap:8px;pointer-events:none';
    document.body.appendChild(c);
  }
  var el = document.createElement('div');
  el.className = 'toast';
  el.textContent = msg;
  c.appendChild(el);
  setTimeout(function () { el.classList.add('out'); setTimeout(function () { el.remove(); }, 300); }, 2700);
}

// ============================================================
// Agent 日志清洗
// ============================================================

/**
 * 清洗 agent job 输出日志，过滤内部协议噪声。
 * 保留 stdout 中有意义的行，stderr 只保留包含错误关键词的行。
 *
 * @param {object} log - { stream, chunk }
 * @returns {string} 清洗后的文本
 */
function cleanAgentLogChunk(log) {
  if (!log || (log.stream !== 'stdout' && log.stream !== 'stderr')) return '';
  var chunk = (log.chunk || '').trim();
  if (!chunk) return '';
  var internal = [
    /^TurnBegin\(/,
    /^TurnEnd\(/,
    /^StepBegin\(/,
    /^StepEnd\(/,
    /^ThinkPart\(/,
    /^ToolUse/,
    /^ToolResult/,
    /^Usage\(/,
    /^TokenUsage/,
    /^To resume this session:/,
    /^OpenAI Codex /,
    /^--------/,
    /^workdir:/,
    /^model:/,
    /^provider:/,
    /^approval:/,
    /^sandbox:/,
    /^reasoning /,
    /^session id:/,
    /^hook:/
  ];
  var nonFatalStderr = [
    /rmcp::transport::worker: worker quit with fatal: Transport channel closed, when Auth\(TokenRefreshFailed/i,
    /invalid_grant: Invalid refresh token/i,
    /codex_core::session: failed to record rollout items: thread .* not found/i
  ];
  var lines = chunk.split(/\r?\n/).map(function (line) { return line.trim(); }).filter(function (line) {
    if (!line) return false;
    if (internal.some(function (re) { return re.test(line); })) return false;
    if (log.stream === 'stderr' && nonFatalStderr.some(function (re) { return re.test(line); })) return false;
    if (log.stream === 'stderr' && !/(error|failed|invalid|not found|panic|denied|timeout)/i.test(line)) return false;
    return true;
  });
  return lines.join('\n');
}

// ============================================================
// 主题切换
// ============================================================

function getTheme() {
  try { return localStorage.getItem('cp_theme') || 'light'; } catch (e) { return 'light'; }
}

function setTheme(theme) {
  try { localStorage.setItem('cp_theme', theme); } catch (e) {}
  document.documentElement.dataset.theme = theme;
  // 图标表示点击后的目标主题，而不是当前主题。
  var icons = document.querySelectorAll('.theme-toggle-icon');
  for (var i = 0; i < icons.length; i++) { icons[i].textContent = theme === 'dark' ? '☀️' : '🌙'; }
}

function toggleTheme() {
  setTheme(getTheme() === 'dark' ? 'light' : 'dark');
}

// ============================================================
// 导出
// ============================================================
if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    escHtml: escHtml,
    esc: esc,
    jsStr: jsStr,
    shortId: shortId,
    trunc: trunc,
    formatTime: formatTime,
    formatMsgTime: formatMsgTime,
    relTime: relTime,
    ago: ago,
    agentLabel: agentLabel,
    shortProject: shortProject,
    projectBasename: projectBasename,
    toast: toast,
    cleanAgentLogChunk: cleanAgentLogChunk,
    getTheme: getTheme,
    setTheme: setTheme,
    toggleTheme: toggleTheme,
  };
}
