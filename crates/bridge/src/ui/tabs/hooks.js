// hooks.js — Hook 配置矩阵视图：agent × event 级别展示和控制
//
// 职责：
// - 加载并渲染全局 hook 配置 + per-agent event 矩阵表
// - 矩阵行展示每个 (agent, event) 的 phase/installed/required/blocking 状态
// - 提供 toggle 开关控制全局 pretooluse 和 agent enabled
// - 提供 reconcile 按钮（全局和 per-agent）

async function loadHooks() {
  const view = document.getElementById('hooks-view');
  if (!view) return;
  view.innerHTML = '<div class="skeleton" style="height:200px"></div>';

  try {
    const data = await api('/hook-status');
    S.hooks = data;
    renderHooks();
  } catch (e) {
    view.innerHTML = '<div class="empty-state">Failed to load hook status</div>';
  }
}

// 阶段 badge 颜色映射
var PHASE_STYLES = {
  before: 'badge-blue',
  permission: 'badge-yellow',
  after: 'badge-green',
  session: 'badge-purple',
  prompt: 'badge-purple',
  turn_end: 'badge-gray'
};

var PHASE_LABELS = {
  before: 'before',
  permission: 'permission',
  after: 'after',
  session: 'session',
  prompt: 'prompt',
  turn_end: 'turn_end'
};

function renderHooks() {
  var view = document.getElementById('hooks-view');
  if (!view || !S.hooks) return;

  var g = S.hooks.global || {};
  var agents = S.hooks.agents || [];

  var html = '<div class="hook-page">';

  // 全局 PreToolUse 评估开关
  html += '<div class="card hook-card">';
  html += '<div class="card-header"><h3>Hook Control</h3></div>';
  html += '<div class="card-body">';
  html += '<div class="setting-row">';
  html += '<span class="setting-label">Global PreToolUse Evaluation</span>';
  html += '<button class="btn btn-sm ' + (g.pretooluse_enabled ? 'btn-success' : 'btn-secondary') + '" onclick="toggleGlobalPretooluse()">' + (g.pretooluse_enabled ? 'ON' : 'OFF') + '</button>';
  html += '</div>';
  html += '<div class="setting-detail">';
  html += '<span class="monospace">' + escHtml(g.config_path || '') + '</span>';
  if (g.hook_binary_path) {
    html += '<br><span class="monospace">' + escHtml(g.hook_binary_path) + '</span>';
  }
  html += '</div>';
  html += '</div></div>';

  // Agent × Event 矩阵表
  for (var ai = 0; ai < agents.length; ai++) {
    var agent = agents[ai];
    var statusBadge = renderHookStatusBadge(agent.status);
    var details = agent.event_details || [];

    html += '<div class="card hook-card">';

    // Agent 头部行：label + status badge + legacy warning + enabled toggle
    html += '<div class="card-header hook-agent-head">';
    html += '<h3>' + escHtml(agent.label || agent.agent) + '</h3> ';
    html += statusBadge;
    if (agent.legacy_present) {
      html += ' <span class="badge badge-warn" title="Legacy hook entries found">⚠ Legacy</span>';
    }
    html += '<span class="hook-head-spacer"></span>';
    html += '<span class="setting-label hook-enabled-label">Enabled</span> ';
    html += '<button class="btn btn-sm ' + (agent.enabled ? 'btn-success' : 'btn-secondary') + '" onclick="toggleAgent(\'' + agent.agent + '\',\'enabled\',' + !agent.enabled + ')">' + (agent.enabled ? 'ON' : 'OFF') + '</button>';
    html += '</div>';

    html += '<div class="card-body">';

    // 矩阵表
    if (details.length > 0) {
      html += '<div class="hook-table-wrap">';
      html += '<table class="hook-matrix">';
      html += '<thead><tr>';
      html += '<th>Event</th>';
      html += '<th>阶段</th>';
      html += '<th class="hook-cell-center">已安装</th>';
      html += '<th class="hook-cell-center">配置</th>';
      html += '<th>策略</th>';
      html += '<th class="hook-cell-center">必需</th>';
      html += '<th class="hook-cell-center">阻断</th>';
      html += '</tr></thead>';
      html += '<tbody>';

      for (var di = 0; di < details.length; di++) {
        var d = details[di];
        var phaseStyle = PHASE_STYLES[d.phase] || 'badge-gray';
        var phaseLabel = PHASE_LABELS[d.phase] || d.phase || '—';
        var installedIcon = d.installed
          ? '<span style="color:#22c55e">✓</span>'
          : '<span style="color:#ef4444">✗</span>';
        var configToggle = '<button class="btn btn-sm ' + (d.config_enabled ? 'btn-success' : 'btn-secondary') + '" onclick="toggleEvent(\'' + agent.agent + '\',\'' + (d.event || '') + '\',' + !d.config_enabled + ')">' + (d.config_enabled ? 'ON' : 'OFF') + '</button>';
        var strategyText = [
          d.decision_strategy ? 'decision=' + d.decision_strategy : '',
          d.completion_strategy ? 'completion=' + d.completion_strategy : '',
          d.timeout_strategy ? 'timeout=' + d.timeout_strategy : ''
        ].filter(Boolean).join(' · ') || 'default';
        var requiredText = d.required ? 'Yes' : '—';
        var blockingText = d.blocking ? 'Yes' : '—';

        html += '<tr>';
        html += '<td><span class="monospace hook-event-name">' + escHtml(d.event || '') + '</span></td>';
        html += '<td><span class="badge ' + phaseStyle + '">' + escHtml(phaseLabel) + '</span></td>';
        html += '<td class="hook-cell-center">' + installedIcon + '</td>';
        html += '<td class="hook-cell-center">' + configToggle + '</td>';
        html += '<td><span class="monospace hook-strategy">' + escHtml(strategyText) + '</span></td>';
        html += '<td class="hook-cell-center">' + requiredText + '</td>';
        html += '<td class="hook-cell-center">' + blockingText + '</td>';
        html += '</tr>';
      }

      html += '</tbody></table></div>';
    } else {
      // fallback：无 event_details 时显示旧格式
      if (agent.installed_events && agent.installed_events.length > 0) {
        html += '<div class="hook-muted-line">已安装: ' + agent.installed_events.join(', ') + '</div>';
      }
      if (agent.missing_events && agent.missing_events.length > 0) {
        html += '<div style="font-size:12px" class="text-warn">缺失: ' + agent.missing_events.join(', ') + '</div>';
      }
    }

    // 配置路径（折叠）
    html += '<details class="hook-config-paths">';
    html += '<summary>配置路径</summary>';
    html += '<div class="monospace hook-config-value">' + escHtml(agent.config_path || '') + ' ' + (agent.config_exists ? '(exists)' : '(missing)') + '</div>';
    if (agent.commands && agent.commands.length > 0) {
      html += '<div class="monospace hook-config-value">' + escHtml(agent.commands[0]) + '</div>';
    }
    html += '</details>';

    html += '</div></div>';
  }

  // 全局 Reconcile 按钮
  html += '<div class="card hook-card">';
  html += '<div class="card-body">';
  html += '<button class="btn btn-primary" onclick="reconcileHooks()">Reconcile Hooks</button>';
  html += '<span class="setting-hint" style="margin-left:8px">同步所有 agent hook 配置到当前状态</span>';
  html += '</div></div>';

  html += '</div>';
  view.innerHTML = html;
}

function renderHookStatusBadge(status) {
  switch (status) {
    case 'ok': return '<span class="badge badge-success">OK</span>';
    case 'disabled': return '<span class="badge badge-secondary">Disabled</span>';
    case 'partial': return '<span class="badge badge-warn">Partial</span>';
    case 'missing_config': return '<span class="badge badge-error">No Config</span>';
    case 'missing_hook_binary': return '<span class="badge badge-error">No Binary</span>';
    default: return '<span class="badge">' + escHtml(status || '') + '</span>';
  }
}

async function toggleGlobalPretooluse() {
  if (!S.hooks) return;
  var newVal = !S.hooks.global.pretooluse_enabled;
  try {
    var res = await api('/hook-config', {
      method: 'POST',
      body: JSON.stringify({ pretooluse_enabled: newVal })
    });
    if (res && !res.error) {
      S.hooks.global.pretooluse_enabled = newVal;
      renderHooks();
    }
  } catch (e) { /* ignore */ }
}

async function toggleAgent(agent, field, value) {
  try {
    var agents = {};
    agents[agent] = {};
    agents[agent][field] = value;
    var res = await api('/hook-config', {
      method: 'POST',
      body: JSON.stringify({ agents: agents })
    });
    if (res && !res.error) {
      // Reload full status
      await loadHooks();
    }
  } catch (e) { /* ignore */ }
}

async function reconcileHooks() {
  try {
    var res = await api('/hook-config', {
      method: 'POST',
      body: JSON.stringify({ reconcile: true })
    });
    if (res && !res.error) {
      await loadHooks();
      if (res.reconcile_reports) {
        var msg = res.reconcile_reports.map(function(r) { return r.agent + ': ' + r.action; }).join('\n');
        alert('Reconcile result:\n' + msg);
      }
    }
  } catch (e) { /* ignore */ }
}

async function toggleEvent(agent, event, value) {
  try {
    var agents = {};
    agents[agent] = { events: {} };
    agents[agent].events[event] = { enabled: value };
    var res = await api('/hook-config', {
      method: 'POST',
      body: JSON.stringify({ agents: agents })
    });
    if (res && !res.error) {
      await loadHooks();
    }
  } catch (e) { /* ignore */ }
}
