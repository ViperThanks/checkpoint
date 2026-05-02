// activity_segment.js — Activity Timeline Compaction
//
// 职责：将连续的工具事件合并成可读的活动摘要。
// bridge 和 relay 统一使用此模块渲染活动时间线。
//
// 核心概念：
// - Segment：一组连续同类工具事件的合并
// - Turn：两个用户消息之间的所有活动（assistant + tools）
// - 完成态："Worked for Xm Ys" 折叠
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
// Tool 分类
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
    var parts = [];
    if (c.file > 0) parts.push(c.file + (c.file === 1 ? ' file' : ' files'));
    if (c.search > 0) parts.push(c.search + (c.search === 1 ? ' search' : ' searches'));
    if (c.command > 0) parts.push(c.command + (c.command === 1 ? ' command' : ' commands'));
    summary = parts.length ? parts.join(' · ') : 'explored';
  } else if (p.category === 'edit') {
    var editCount = c.edit || c.file || p.items.length;
    summary = editCount + (editCount === 1 ? ' edit' : ' edits');
  }

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
    summary: m.tool_name || '工具'
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
 * @param {Segment} seg
 * @returns {string} HTML
 */
function renderSegmentDetail(seg) {
  if (!seg.items || !seg.items.length) return '';
  var html = '';
  for (var i = 0; i < seg.items.length; i++) {
    var item = seg.items[i];
    var name = escHtml(item.tool_name || 'tool');
    var preview = item.tool_input_preview ? escHtml(item.tool_input_preview) : '';
    html += '<div class="act-item">';
    html += '<span class="act-item-name">' + name + '</span>';
    if (preview) html += '<code class="act-item-preview">' + preview + '</code>';
    html += '</div>';
  }
  return html;
}

// ============================================================
// 渲染 — Turn 标题
// ============================================================

/**
 * 渲染 Turn 组的标题栏。
 * 完成态："Worked for 3m 21s" + 工具数量 + 折叠切换
 * 运行中态：脉冲指示 + "Working" + 当前耗时
 *
 * @param {TurnGroup} group
 * @param {number} idx - turn 索引
 * @returns {string} HTML
 */
function renderTurnBanner(group, idx) {
  if (!group.toolCount && group.segments.length === 0) return '';

  var cls = group.isRunning ? 'act-turn act-running' : 'act-turn act-settled';
  var html = '<div class="' + cls + '" data-turn-idx="' + idx + '">';

  // 标题栏
  html += '<div class="act-turn-bar"' + (!group.isRunning ? ' onclick="toggleTurnBody(this)"' : '') + '>';

  if (group.isRunning) {
    html += '<span class="act-pulse"></span>';
    html += '<span class="act-turn-label">Working ' + escHtml(formatDuration(group.duration || 1)) + '</span>';
  } else {
    html += '<span class="act-turn-label">Worked for ' + escHtml(formatDuration(group.duration || 1)) + '</span>';
  }

  if (group.toolCount > 0) {
    html += '<span class="act-turn-tools">Ran ' + group.toolCount + ' command' + (group.toolCount === 1 ? '' : 's') + '</span>';
  }

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
  };
}
