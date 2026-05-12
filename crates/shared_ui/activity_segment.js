// activity_segment.js — Activity Timeline Compaction
//
// 职责：将连续的工具事件合并成可读的活动摘要。
// bridge 和 relay 统一使用此模块渲染活动时间线。
//
// 核心概念：
// - Segment：一组连续同类工具事件的合并
// - Turn：两个用户消息之间的所有活动（assistant + tools）
// - 完成态："耗时 Xm Ys" 折叠
// - 运行中态：实时累积摘要 + 脉冲指示
//
// 依赖：view_model.js (escHtml, jsStr)

// Node.js 环境：加载依赖（浏览器环境由注入顺序保证）
if (typeof module !== 'undefined' && module.exports) {
  var vm = require('./view_model.js');
  var escHtml = vm.escHtml;
  var jsStr = vm.jsStr;
}

// ============================================================
// Tool 分类 + 显示名
// ============================================================

/**
 * 工具分类表：tool_name → [category, subcategory]
 * - category: 'explore' | 'edit'
 * - subcategory: 'file' | 'search' | 'command' | 'edit'
 *
 * explore 包含文件读取、搜索、命令执行（都属于"探索/理解代码"阶段）。
 * edit 只包含文件修改操作。
 */
var TOOL_MAP = {
  // Claude Code
  'Read':           ['explore', 'file'],
  'LS':             ['explore', 'file'],
  'LSP':            ['explore', 'file'],
  'Glob':           ['explore', 'search'],
  'Grep':           ['explore', 'search'],
  'WebFetch':       ['explore', 'search'],
  'WebSearch':      ['explore', 'search'],
  'Bash':           ['explore', 'command'],
  'Shell':          ['explore', 'command'],
  'Edit':           ['edit', 'file'],
  'Write':          ['edit', 'file'],
  'NotebookEdit':   ['edit', 'file'],
  // Codex CLI
  'exec_command':   ['explore', 'command'],
  'write_stdin':    ['explore', 'command'],
  'ReadFile':       ['explore', 'file'],
  'List':           ['explore', 'file'],
  'web_search':     ['explore', 'search'],
  'ApplyPatch':     ['edit', 'file'],
  // Kimi Code
  'WriteFile':      ['edit', 'file'],
  'StrReplaceFile': ['edit', 'file'],
  // Gemini CLI
  'read_file':      ['explore', 'file'],
  'write_file':     ['edit', 'file'],
  'run_shell_command': ['explore', 'command'],
};

/**
 * 工具技术名 → 显示名。未列出时回退到原 tool_name。
 */
var DISPLAY_NAMES = {
  'Read': '读取文件', 'LS': '列出目录', 'LSP': 'LSP',
  'Glob': '搜索文件', 'Grep': '搜索内容',
  'WebFetch': '获取网页', 'WebSearch': '搜索网络',
  'Bash': '运行命令', 'Shell': '运行命令',
  'Edit': '编辑文件', 'Write': '写入文件', 'NotebookEdit': '编辑笔记本',
  'exec_command': '运行命令', 'write_stdin': '输入命令',
  'ReadFile': '读取文件', 'List': '列出目录',
  'web_search': '搜索网络', 'ApplyPatch': '应用补丁',
  'WriteFile': '写入文件', 'StrReplaceFile': '编辑文件',
  'read_file': '读取文件', 'write_file': '写入文件', 'run_shell_command': '运行命令',
};

/**
 * 获取工具的分类信息。
 * @param {string} toolName
 * @returns {{ category: string, subcategory: string }|null}
 */
function classifyTool(toolName) {
  if (!toolName) return null;
  var entry = TOOL_MAP[toolName];
  if (!entry) return null;
  return { category: entry[0], subcategory: entry[1] };
}

// ============================================================
// 命令/路径提取 + 脱敏
// ============================================================

/**
 * 从 preview 中提取文件 basename（最后一段路径）。
 * "src/main.rs" → "main.rs"
 * "/Users/x/proj/lib.rs" → "lib.rs"
 */
function extractFileName(preview) {
  if (!preview) return '';
  var s = preview.trim();
  // 去掉常见前缀标记
  s = s.replace(/^(file_path:?\s*|path:?\s*|reading:?\s*)/i, '');
  // 取最后一段
  var parts = s.split('/');
  var last = parts[parts.length - 1] || s;
  // 截断过长名字
  if (last.length > 40) last = last.substring(0, 37) + '...';
  return last || s;
}

/**
 * 从 tool event 提取可读标签和详情。
 */
function extractCommand(item) {
  var preview = item.tool_input_preview || '';
  var tool = item.tool_name || '';
  var info = classifyTool(tool);
  var label = DISPLAY_NAMES[tool] || tool;

  if (!info) return { label: label, detail: preview };
  var sub = info.subcategory;

  if (sub === 'file' || sub === 'edit') {
    return { label: label, detail: extractFileName(preview) };
  }
  if (sub === 'command') {
    return { label: label, detail: preview };
  }
  if (sub === 'search') {
    return { label: label, detail: preview };
  }
  return { label: label, detail: preview };
}

/**
 * 对 preview 文本中的敏感值做脱敏。
 * 长 hex token、password=xxx、token=xxx 等替换为 ****。
 */
var SENSITIVE_PATTERNS = [
  /\b([A-Za-z0-9]{40,})\b/g,
  /\b(password\s*[=:]\s*)\S+/gi,
  /\b(token\s*[=:]\s*)\S+/gi,
  /\b(secret\s*[=:]\s*)\S+/gi,
];

function maskSensitive(text) {
  if (!text) return text;
  var result = text;
  // 长 hex string
  result = result.replace(/\b([A-Za-z0-9]{40,})\b/g, '****');
  // key=value 敏感字段：保留 key，mask value
  result = result.replace(/\b(password\s*[=:]\s*)\S+/gi, '$1****');
  result = result.replace(/\b(token\s*[=:]\s*)\S+/gi, '$1****');
  result = result.replace(/\b(secret\s*[=:]\s*)\S+/gi, '$1****');
  return result;
}

// ============================================================
// Duration 格式化
// ============================================================

/**
 * 格式化毫秒为人类可读时长。
 * @param {number} ms
 * @returns {string} "21s" | "3m 21s" | "1h 2m"
 */
function formatDuration(ms) {
  if (!ms || ms < 0) return '';
  var secs = Math.floor(ms / 1000);
  if (secs < 1) return '< 1s';
  if (secs < 60) return secs + 's';
  var mins = Math.floor(secs / 60);
  var remainSecs = secs % 60;
  if (mins < 60) return mins + 'm ' + remainSecs + 's';
  var hours = Math.floor(mins / 60);
  var remainMins = mins % 60;
  return hours + 'h ' + remainMins + 'm';
}

// ============================================================
// Turn 工作阶段分类
// ============================================================

/**
 * 根据 turn 内 segment 组合推断工作阶段。
 * 返回中文标签，用于 turn banner 前缀。
 *
 * 分类逻辑：
 * - 只有文件读取/搜索 → 理解任务
 * - 只有命令 → 执行命令
 * - 命令 + 文件/搜索 → 检查状态
 * - 只有编辑 → 修改文件
 * - 编辑 + 命令 → 验证结果
 * - 编辑 + 文件/搜索（无命令）→ 修改文件
 */
var PHASE_LABELS = {
  'understand': '理解上下文',
  'diagnose':   '检查状态',
  'execute':    '执行命令',
  'edit':       '修改文件',
  'verify':     '验证结果',
};

function classifyTurnPhase(segments) {
  var hasFile = false, hasSearch = false, hasCommand = false, hasEdit = false;
  for (var i = 0; i < segments.length; i++) {
    var seg = segments[i];
    if (seg.type === 'explore') {
      if (seg.fileCount > 0) hasFile = true;
      if (seg.searchCount > 0) hasSearch = true;
      if (seg.commandCount > 0) hasCommand = true;
    } else if (seg.type === 'edit') {
      hasEdit = true;
    }
  }

  if (!hasEdit && !hasCommand && (hasFile || hasSearch)) return 'understand';
  if (hasEdit && hasCommand) return 'verify';
  if (hasEdit) return 'edit';
  if (hasCommand && !hasFile && !hasSearch) return 'execute';
  if (hasCommand) return 'diagnose';
  if (hasFile || hasSearch) return 'understand';
  return '';
}

/**
 * 生成 turn banner 的标签 HTML（供 bridge/relay 渲染器使用）。
 * 包含阶段标签 + 时长 + 动作数。
 */
function turnBannerLabel(group) {
  var html = '';
  var durStr = formatDuration(group.duration || 1);
  if (group.isRunning) {
    html += '<span class="act-pulse"></span>';
    html += '<span class="act-turn-label">处理中' + (durStr ? ' ' + durStr : '') + '</span>';
  } else {
    html += '<span class="act-turn-label">耗时' + (durStr ? ' ' + durStr : '') + '</span>';
  }
  if (group.toolCount > 0) {
    html += '<span class="act-turn-tools">' + group.toolCount + ' 次操作</span>';
  }
  return html;
}

// ============================================================
// Segment 构建
// ============================================================

/**
 * 从扁平消息数组构建活动段。
 * 连续同类工具事件合并为一个段。用户/助手消息保留为独立段。
 *
 * @param {Array} messages - { role, tool_name, tool_input_preview, tool_input_full, timestamp, seq }
 * @returns {Array<Segment>}
 *
 * Segment 类型：
 *   { type: 'user'|'assistant', message: object }
 *   { type: 'explore'|'edit', items: [object], startTime, endTime, duration,
 *     fileCount, searchCount, commandCount, editCount, summary }
 */
function buildSegments(messages) {
  if (!messages || !messages.length) return [];
  var segments = [];
  var pending = null;

  for (var i = 0; i < messages.length; i++) {
    var m = messages[i];

    if (m.role === 'user' || m.role === 'assistant') {
      if (pending) { segments.push(_finalizeSeg(pending)); pending = null; }
      segments.push({ type: m.role, message: m });
      continue;
    }

    if (m.role !== 'tool_summary') continue;

    var info = classifyTool(m.tool_name);
    if (!info) {
      // 未知/内部工具 — 单独通过
      segments.push(_singleToolSeg(m));
      continue;
    }

    var cat = info.category;
    var sub = info.subcategory;

    if (pending && pending.category === cat) {
      // 同类 → 合并
      pending.items.push(m);
      if (m.timestamp) pending.endTime = m.timestamp;
      pending.counts[sub] = (pending.counts[sub] || 0) + 1;
      continue;
    }

    // 不同类 → 刷出旧段，开始新段
    if (pending) segments.push(_finalizeSeg(pending));
    pending = { category: cat, items: [m], startTime: m.timestamp, endTime: m.timestamp || null, counts: {} };
    pending.counts[sub] = 1;
  }

  if (pending) segments.push(_finalizeSeg(pending));
  return segments;
}

function _finalizeSeg(p) {
  var startMs = p.startTime ? new Date(p.startTime).getTime() : 0;
  var endMs = p.endTime ? new Date(p.endTime).getTime() : 0;
  var dur = endMs > startMs ? endMs - startMs : 0;
  var c = p.counts;
  var summary = '';

  if (p.category === 'explore') {
    var totalOps = (c.file || 0) + (c.search || 0) + (c.command || 0);
    if (totalOps === 1) {
      // 单操作：用真实文件名/命令
      var item = p.items[0];
      var cmd = extractCommand(item);
      summary = cmd.label + ' ' + cmd.detail;
    } else if (totalOps <= 3) {
      // 少量：列出每个操作
      var parts = [];
      for (var i = 0; i < p.items.length; i++) {
        var ci = extractCommand(p.items[i]);
        var shortDetail = ci.detail.length > 25 ? ci.detail.substring(0, 22) + '...' : ci.detail;
        parts.push(shortDetail);
      }
      summary = parts.join(' · ');
    } else {
      // 多操作：分类计数
      var countParts = [];
      if (c.file > 0) countParts.push('读取 ' + c.file + ' 个文件');
      if (c.search > 0) countParts.push('搜索 ' + c.search + ' 次');
      if (c.command > 0) countParts.push('运行 ' + c.command + ' 条命令');
      summary = countParts.join(' · ');
    }
  } else if (p.category === 'edit') {
    var editCount = c.edit || c.file || p.items.length;
    if (editCount === 1) {
      var editItem = p.items[0];
      var editCmd = extractCommand(editItem);
      summary = '编辑 ' + editCmd.detail;
    } else {
      summary = '修改 ' + editCount + ' 个文件';
    }
  }

  // 折叠摘要也要脱敏
  summary = maskSensitive(summary);

  return {
    type: p.category,
    items: p.items,
    startTime: p.startTime,
    endTime: p.endTime,
    duration: dur,
    fileCount: c.file || 0,
    searchCount: c.search || 0,
    commandCount: c.command || 0,
    editCount: c.edit || c.file || p.items.length,
    summary: summary
  };
}

function _singleToolSeg(m) {
  return {
    type: 'tool',
    items: [m],
    startTime: m.timestamp,
    endTime: m.timestamp,
    duration: 0,
    fileCount: 0, searchCount: 0, commandCount: 0, editCount: 0,
    summary: m.tool_name || 'tool'
  };
}

// ============================================================
// Turn 分组
// ============================================================

/**
 * 将段列表分组为 Turn（两个用户消息之间的所有活动）。
 *
 * @param {Array<Segment>} segments
 * @param {boolean} isLastRunning - 最后一个 turn 是否仍在进行
 * @returns {Array<TurnGroup>}
 *
 * TurnGroup: {
 *   userSeg: Segment|null,
 *   segments: Segment[],
 *   startTime, endTime, duration,
 *   isRunning: boolean,
 *   toolCount: number
 * }
 */
function buildTurnGroups(segments, isLastRunning) {
  if (!segments || !segments.length) return [];
  var groups = [];
  var current = null;

  for (var i = 0; i < segments.length; i++) {
    var seg = segments[i];

    if (seg.type === 'user') {
      if (current) groups.push(current);
      current = { userSeg: seg, segments: [], startTime: null, endTime: null, duration: 0, isRunning: false, toolCount: 0 };
      continue;
    }

    // 没有 user 开头的内容（例如 conversation 开头的 assistant 消息）
    if (!current) {
      current = { userSeg: null, segments: [], startTime: null, endTime: null, duration: 0, isRunning: false, toolCount: 0 };
    }

    current.segments.push(seg);

    // 更新时间范围
    var segTime = seg.startTime || (seg.message && seg.message.timestamp);
    if (segTime && (!current.startTime || segTime < current.startTime)) current.startTime = segTime;
    var segEnd = seg.endTime || (seg.message && seg.message.timestamp);
    if (segEnd && (!current.endTime || segEnd > current.endTime)) current.endTime = segEnd;

    // 计算工具数量
    if (seg.items) current.toolCount += seg.items.length;
  }

  if (current) groups.push(current);

  // 计算持续时间和标记运行状态
  for (var j = 0; j < groups.length; j++) {
    var g = groups[j];
    var sMs = g.startTime ? new Date(g.startTime).getTime() : 0;
    var eMs = g.endTime ? new Date(g.endTime).getTime() : 0;
    g.duration = eMs > sMs ? eMs - sMs : 0;
    g.isRunning = (j === groups.length - 1) && !!isLastRunning;
  }

  return groups;
}

// ============================================================
// Tool Run 切分
// ============================================================

/**
 * 将一个 TurnGroup 按 assistant/user 边界切分为独立 tool run。
 *
 * 每个 run 是一段连续的非聊天 segment，拥有独立的
 * segments / toolCount / startTime / endTime / duration / isRunning。
 * 可直接传给 turnBannerLabel / renderTurnBanner。
 *
 * ToolRun: {
 *   segments: Segment[],  // 只有 tool segments (explore/edit/tool)
 *   toolCount: number,
 *   startTime, endTime, duration,
 *   isRunning: boolean
 * }
 *
 * @param {TurnGroup} group
 * @returns {Array<ToolRun>}
 */
function buildToolRuns(group) {
  if (!group || !group.segments || !group.segments.length) return [];
  var runs = [];
  var current = null;

  for (var i = 0; i < group.segments.length; i++) {
    var seg = group.segments[i];
    var isChat = (seg.type === 'user' || seg.type === 'assistant');

    if (isChat) {
      // chat segment → 关闭当前 run
      if (current) { runs.push(current); current = null; }
      continue;
    }

    // tool segment → 累积到当前 run
    if (!current) {
      current = { segments: [], toolCount: 0, startTime: null, endTime: null, duration: 0, isRunning: false };
    }
    current.segments.push(seg);
    if (seg.items) current.toolCount += seg.items.length;

    var segStart = seg.startTime;
    var segEnd = seg.endTime;
    if (segStart && (!current.startTime || segStart < current.startTime)) current.startTime = segStart;
    if (segEnd && (!current.endTime || segEnd > current.endTime)) current.endTime = segEnd;
  }

  if (current) runs.push(current);

  // 计算 duration 和 isRunning
  for (var j = 0; j < runs.length; j++) {
    var r = runs[j];
    var sMs = r.startTime ? new Date(r.startTime).getTime() : 0;
    var eMs = r.endTime ? new Date(r.endTime).getTime() : 0;
    r.duration = eMs > sMs ? eMs - sMs : 0;
    // 只有 turn 整体 running 且这是最后一个 run 时标记 running
    r.isRunning = group.isRunning && (j === runs.length - 1);
  }

  return runs;
}

// ============================================================
// 渲染 — Segment 摘要卡片
// ============================================================

/**
 * 渲染工具段的摘要卡片。
 * @param {Segment} seg
 * @param {number} idx - 段索引，用于展开/折叠
 * @returns {string} HTML
 */
function renderSegmentCard(seg, idx) {
  if (seg.type === 'user' || seg.type === 'assistant') return '';
  var catClass = seg.type === 'explore' ? 'act-explore' : (seg.type === 'edit' ? 'act-edit' : 'act-tool');
  var itemCount = seg.items ? seg.items.length : 0;
  var expandable = itemCount > 0;
  var idxStr = String(idx);

  var html = '<div class="act-card ' + catClass + '" data-act-idx="' + escHtml(idxStr) + '">';

  // 摘要行
  html += '<div class="act-summary"' + (expandable ? ' onclick="toggleActDetail(this)"' : '') + '>';
  html += '<span class="act-summary-text">' + escHtml(seg.summary) + '</span>';
  if (expandable) {
    html += '<span class="act-chevron" id="act-chevron-' + escHtml(idxStr) + '">&#x25B8;</span>';
  }
  html += '</div>';

  // 工具段默认压缩，点击摘要行展开详情；单个命令也保持同一交互模型。
  if (expandable) {
    html += '<div class="act-detail" id="act-detail-' + escHtml(idxStr) + '" style="display:none">';
    html += renderSegmentDetail(seg);
    html += '</div>';
  }

  html += '</div>';
  return html;
}

/**
 * 渲染段内工具条目列表（展开时显示）。
 * 使用中文工具名 + 脱敏后的 preview。
 */
function renderSegmentDetail(seg) {
  if (!seg.items || !seg.items.length) return '';
  var html = '';
  for (var i = 0; i < seg.items.length; i++) {
    var item = seg.items[i];
    var cmd = extractCommand(item);
    var label = escHtml(cmd.label);
    var detail = cmd.detail ? escHtml(maskSensitive(cmd.detail)) : '';
    html += '<div class="act-item">';
    html += '<span class="act-item-name">' + label + '</span>';
    if (detail) html += '<code class="act-item-preview">' + detail + '</code>';
    html += '</div>';
  }
  return html;
}

// ============================================================
// 渲染 — Turn 标题
// ============================================================

/**
 * 渲染 Turn 组的完整标题栏 + 段卡片。
 * 使用 turnBannerLabel 生成统一的标签。
 */
function renderTurnBanner(group, idx) {
  if (!group.toolCount && group.segments.length === 0) return '';

  var cls = group.isRunning ? 'act-turn act-running' : 'act-turn act-settled';
  var html = '<div class="' + cls + '" data-turn-idx="' + idx + '">';

  // 标题栏 — 使用统一的标签生成器
  html += '<div class="act-turn-bar"' + (!group.isRunning ? ' onclick="toggleTurnBody(this)"' : '') + '>';
  html += turnBannerLabel(group);

  if (!group.isRunning && group.toolCount > 0) {
    html += '<span class="act-turn-chevron" id="act-turn-chevron-' + idx + '" style="display:none">&#x25B8;</span>';
  }

  html += '</div>'; // .act-turn-bar

  // Body（展开/折叠）
  var bodyStyle = group.isRunning ? '' : ' style="display:none"';
  html += '<div class="act-turn-body" id="act-turn-body-' + idx + '"' + bodyStyle + '>';

  for (var i = 0; i < group.segments.length; i++) {
    var seg = group.segments[i];
    if (seg.type === 'user' || seg.type === 'assistant') continue;
    html += renderSegmentCard(seg, idx + '-' + i);
  }

  html += '</div>'; // .act-turn-body
  html += '</div>'; // .act-turn
  return html;
}

// ============================================================
// DOM 交互
// ============================================================

/** 切换工具段详情展开/折叠 */
function toggleActDetail(idx) {
  var detail;
  var chevron;
  if (idx && idx.nodeType === 1) {
    var card = idx.closest ? idx.closest('.act-card') : null;
    detail = card ? card.querySelector('.act-detail') : null;
    chevron = card ? card.querySelector('.act-chevron') : null;
  } else {
    detail = document.getElementById('act-detail-' + idx);
    chevron = document.getElementById('act-chevron-' + idx);
  }
  if (!detail) return;
  var open = detail.style.display !== 'none';
  detail.style.display = open ? 'none' : '';
  if (chevron) chevron.innerHTML = open ? '&#x25B8;' : '&#x25BE;';
}

/** 切换 Turn 折叠/展开 */
function toggleTurnBody(idx) {
  var body;
  var chevron;
  if (idx && idx.nodeType === 1) {
    var turn = idx.closest ? idx.closest('.act-turn') : null;
    body = turn ? turn.querySelector('.act-turn-body') : null;
    chevron = turn ? turn.querySelector('.act-turn-chevron') : null;
  } else {
    body = document.getElementById('act-turn-body-' + idx);
    chevron = document.getElementById('act-turn-chevron-' + idx);
  }
  if (!body) return;
  var open = body.style.display !== 'none';
  body.style.display = open ? 'none' : '';
  if (chevron) {
    chevron.style.display = open ? 'none' : '';
    chevron.innerHTML = open ? '&#x25B8;' : '&#x25BE;';
  }
}

// ============================================================
// 导出
// ============================================================
if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    classifyTool: classifyTool,
    formatDuration: formatDuration,
    buildSegments: buildSegments,
    buildTurnGroups: buildTurnGroups,
    renderSegmentCard: renderSegmentCard,
    renderSegmentDetail: renderSegmentDetail,
    renderTurnBanner: renderTurnBanner,
    toggleActDetail: toggleActDetail,
    toggleTurnBody: toggleTurnBody,
    extractFileName: extractFileName,
    extractCommand: extractCommand,
    maskSensitive: maskSensitive,
    classifyTurnPhase: classifyTurnPhase,
    turnBannerLabel: turnBannerLabel,
    buildToolRuns: buildToolRuns,
  };
}
