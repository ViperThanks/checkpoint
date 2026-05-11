// runtime_health.js — 共享 Runtime Health UI 层
//
// 职责：运行环境健康状态的 UI 渲染。
// bridge 和 relay 统一使用此模块，不允许各自内联 health badge/banner/alert 逻辑。
//
// 核心可见性要求：
// - critical 状态必须在首屏可见（alert card / banner）
// - 409 runtime_drift 时两端都显示确认弹窗
// - force_confirm 只在用户确认后发送
//
// 依赖：view_model.js 中的 escHtml, jsStr（在注入顺序中排在本文件之前）

// Node.js 环境：加载依赖（浏览器环境由注入顺序保证）
if (typeof module !== 'undefined' && module.exports) {
  var vm = require('./view_model.js');
  var escHtml = vm.escHtml;
  var jsStr = vm.jsStr;
  var permissionModeLabel = vm.permissionModeLabel;
}

// ============================================================
// Runtime Health Badge
// ============================================================

/**
 * 生成运行环境健康状态 badge HTML。
 * ok/unknown/null 时不显示。
 *
 * @param {object|null} health - { status, warnings? }
 * @returns {string} HTML 或空字符串
 */
function runtimeHealthBadge(health) {
  if (!health || !health.status || health.status === 'unknown' || health.status === 'ok') return '';
  if (health.status === 'critical') return '<span class="badge badge-red">环境漂移</span>';
  if (health.status === 'warning') return '<span class="badge badge-yellow">环境变更</span>';
  return '';
}

// ============================================================
// Runtime Alert Card（首屏告警）
// ============================================================

/**
 * 生成运行环境漂移告警卡片。
 * 用于首页和会话列表顶部，确保 critical 状态首屏可见。
 *
 * @param {Array} convos - 会话列表
 * @returns {string} HTML 或空字符串
 */
function runtimeAlertCard(convos) {
  var critical = (convos || []).filter(function (c) {
    return c.runtime_health && c.runtime_health.status === 'critical';
  });
  if (!critical.length) return '';
  var first = critical[0];
  var more = critical.length > 1 ? '，另有 ' + (critical.length - 1) + ' 个' : '';
  return '<div class="runtime-alert-card" onclick="openConvDetail(\'' + jsStr(first.id) + '\')">' +
    '<div class="runtime-alert-title">运行环境漂移' + more + '</div>' +
    '<div class="runtime-alert-sub">' + escHtml(first.title || first.conversation_id || '未命名会话') + '</div>' +
    '<div class="runtime-alert-action">点此查看详情</div>' +
    '</div>';
}

// ============================================================
// Runtime Health Banner（会话详情内联）
// ============================================================

/**
 * 生成会话详情页顶部的健康状态 banner。
 * critical 为红色，warning 为黄色。
 *
 * @param {object} conv - 会话对象，含 runtime_health
 * @returns {string} HTML 或空字符串
 */
function runtimeHealthBanner(conv) {
  var health = conv && conv.runtime_health;
  if (!health || !health.status || health.status === 'unknown' || health.status === 'ok') return '';
  var critical = health.status === 'critical';
  var text = critical ? '运行环境已漂移，继续前需要确认' : '运行环境有变更';
  var cls = critical ? 'runtime-critical' : 'runtime-warning';
  return '<div class="runtime-banner ' + cls + '">' +
    '<div><strong>' + text + '</strong>' + runtimeHealthSummary(health) + '</div>' +
    '<button class="runtime-check-btn" onclick="checkRuntimeHealth(\'' + conv.id + '\')">检查</button>' +
    '</div>';
}

// ============================================================
// Runtime Health Summary
// ============================================================

/**
 * 生成健康警告的详细摘要。
 *
 * @param {object} health
 * @returns {string} HTML
 */
function runtimeHealthSummary(health) {
  var warnings = (health && health.warnings) || [];
  if (!warnings.length) return '';
  return '<div class="runtime-summary">' + warnings.map(function (w) {
    var field = w.field || 'runtime';
    var oldVal = w.expected || w.stored || w.recorded || '-';
    var newVal = w.actual || w.current || '-';
    oldVal = formatRuntimeValue(field, oldVal);
    newVal = formatRuntimeValue(field, newVal);
    return escHtml(field + ': ' + oldVal + ' → ' + newVal);
  }).join('<br>') + '</div>';
}

/**
 * 根据字段类型格式化 runtime 值。
 *
 * @param {string} field
 * @param {string} value
 * @returns {string}
 */
function formatRuntimeValue(field, value) {
  var key = String(field || '').toLowerCase();
  if (key === 'permission_mode' || key === 'permissionmode') {
    return typeof permissionModeLabel === 'function' ? permissionModeLabel(value) : value;
  }
  return value;
}

// ============================================================
// Drift Text（用于 confirm 弹窗）
// ============================================================

/**
 * 生成运行环境漂移的纯文本描述。
 * 用于 409 确认弹窗中的提示信息。
 *
 * @param {object} health
 * @returns {string}
 */
function driftText(health) {
  var warnings = (health && health.warnings) || [];
  if (!warnings.length) return '检测到运行环境不一致。';
  return warnings.map(function (w) {
    var field = w.field || 'runtime';
    var oldVal = w.expected || w.stored || w.recorded || '-';
    var newVal = w.actual || w.current || '-';
    oldVal = formatRuntimeValue(field, oldVal);
    newVal = formatRuntimeValue(field, newVal);
    return field + ': ' + oldVal + ' → ' + newVal;
  }).join('\n');
}

// ============================================================
// Parse Runtime Health（从 API 响应提取）
// ============================================================

/**
 * 从 API 响应数据中解析 runtime_health 字段。
 * 兼容不同的响应格式。
 *
 * @param {object} data - API 响应
 * @returns {object|null}
 */
function parseRuntimeHealth(data) {
  if (!data) return null;
  return data.runtime_health || data.health || null;
}

// ============================================================
// Runtime Drift Confirm（409 确认弹窗）
// ============================================================

/**
 * 显示运行环境漂移确认弹窗。
 * 返回用户是否确认继续。
 *
 * @param {object} runtimeHealth
 * @returns {boolean}
 */
function confirmRuntimeDrift(runtimeHealth) {
  return confirm('运行环境已漂移，可能会接错模型/权限/工具链。\n\n' +
    driftText(runtimeHealth) + '\n\n仍然继续？');
}

// ============================================================
// 导出
// ============================================================
if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    runtimeHealthBadge: runtimeHealthBadge,
    runtimeAlertCard: runtimeAlertCard,
    runtimeHealthBanner: runtimeHealthBanner,
    runtimeHealthSummary: runtimeHealthSummary,
    formatRuntimeValue: formatRuntimeValue,
    driftText: driftText,
    parseRuntimeHealth: parseRuntimeHealth,
    confirmRuntimeDrift: confirmRuntimeDrift,
  };
}
