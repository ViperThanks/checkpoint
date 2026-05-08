// hooks.js — Hook 配置状态展示和开关控制
//
// 职责：
// - 加载并渲染全局和 per-agent hook 配置状态
// - 提供 toggle 开关控制全局 pretooluse 和 agent 开关
// - 提供 reconcile 按钮

async function loadHooks() {
  const view = document.getElementById('hooks-view');
  if (!view) return;
  view.innerHTML = '<div class="skeleton" style="height:200px"></div>';

  try {
    const res = await api('/hook-status');
    const data = await res.json();
    S.hooks = data;
    renderHooks();
  } catch (e) {
    view.innerHTML = '<div class="empty-state">Failed to load hook status</div>';
  }
}

function renderHooks() {
  const view = document.getElementById('hooks-view');
  if (!view || !S.hooks) return;

  const g = S.hooks.global;
  const agents = S.hooks.agents || [];

  let html = '<div class="section">';

  // 全局 PreToolUse 评估开关
  html += '<div class="card">';
  html += '<div class="card-header"><h3>Hook Control</h3></div>';
  html += '<div class="card-body">';
  html += '<div class="setting-row">';
  html += '<span class="setting-label">Global PreToolUse Evaluation</span>';
  html += `<button class="btn btn-sm ${g.pretooluse_enabled ? 'btn-success' : 'btn-secondary'}" onclick="toggleGlobalPretooluse()">${g.pretooluse_enabled ? 'ON' : 'OFF'}</button>`;
  html += '</div>';
  html += '<div class="setting-detail">';
  html += `<span class="monospace">${escHtml(g.config_path || '')}</span>`;
  if (g.hook_binary_path) {
    html += `<br><span class="monospace">${escHtml(g.hook_binary_path)}</span>`;
  }
  html += '</div>';
  html += '</div></div>';

  // Agent 行
  for (const agent of agents) {
    const statusBadge = renderHookStatusBadge(agent.status);
    html += '<div class="card">';
    html += '<div class="card-header">';
    html += `<h3>${escHtml(agent.label)}</h3>`;
    html += ` ${statusBadge}`;
    html += '</div>';
    html += '<div class="card-body">';

    // 开关行
    html += '<div class="setting-row">';
    html += '<span class="setting-label">Enabled</span>';
    html += `<button class="btn btn-sm ${agent.enabled ? 'btn-success' : 'btn-secondary'}" onclick="toggleAgent('${agent.agent}','enabled',${!agent.enabled})">${agent.enabled ? 'ON' : 'OFF'}</button>`;
    html += '</div>';

    html += '<div class="setting-row">';
    html += '<span class="setting-label">PreToolUse</span>';
    html += `<button class="btn btn-sm ${agent.pretooluse_enabled ? 'btn-success' : 'btn-secondary'}" onclick="toggleAgent('${agent.agent}','pretooluse_enabled',${!agent.pretooluse_enabled})">${agent.pretooluse_enabled ? 'ON' : 'OFF'}</button>`;
    html += '</div>';

    html += '<div class="setting-row">';
    html += '<span class="setting-label">Metadata</span>';
    html += `<button class="btn btn-sm ${agent.metadata_enabled ? 'btn-success' : 'btn-secondary'}" onclick="toggleAgent('${agent.agent}','metadata_enabled',${!agent.metadata_enabled})">${agent.metadata_enabled ? 'ON' : 'OFF'}</button>`;
    html += '</div>';

    html += '<div class="setting-row">';
    html += '<span class="setting-label">Stop</span>';
    html += `<button class="btn btn-sm ${agent.stop_enabled ? 'btn-success' : 'btn-secondary'}" onclick="toggleAgent('${agent.agent}','stop_enabled',${!agent.stop_enabled})">${agent.stop_enabled ? 'ON' : 'OFF'}</button>`;
    html += '</div>';

    // 配置路径
    html += '<div class="setting-detail">';
    html += `<span class="monospace">${escHtml(agent.config_path || '')} ${agent.config_exists ? '(exists)' : '(missing)'}</span>`;
    if (agent.commands && agent.commands.length > 0) {
      html += `<br><span class="monospace">${escHtml(agent.commands[0])}</span>`;
    }
    if (agent.installed_events && agent.installed_events.length > 0) {
      html += `<br>Installed: ${agent.installed_events.join(', ')}`;
    }
    if (agent.missing_events && agent.missing_events.length > 0) {
      html += `<br><span class="text-warn">Missing: ${agent.missing_events.join(', ')}</span>`;
    }
    html += '</div>';

    html += '</div></div>';
  }

  // Reconcile 按钮
  html += '<div class="card">';
  html += '<div class="card-body">';
  html += '<button class="btn btn-primary" onclick="reconcileHooks()">Reconcile Hooks</button>';
  html += '<span class="setting-hint">Add/remove hook entries per current config</span>';
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
    default: return `<span class="badge">${escHtml(status)}</span>`;
  }
}

async function toggleGlobalPretooluse() {
  if (!S.hooks) return;
  const newVal = !S.hooks.global.pretooluse_enabled;
  try {
    const res = await api('/hook-config', {
      method: 'POST',
      body: JSON.stringify({ pretooluse_enabled: newVal })
    });
    if (res.ok) {
      S.hooks.global.pretooluse_enabled = newVal;
      renderHooks();
    }
  } catch (e) { /* ignore */ }
}

async function toggleAgent(agent, field, value) {
  try {
    const agents = {};
    agents[agent] = {};
    agents[agent][field] = value;
    const res = await api('/hook-config', {
      method: 'POST',
      body: JSON.stringify({ agents })
    });
    if (res.ok) {
      // Reload full status
      await loadHooks();
    }
  } catch (e) { /* ignore */ }
}

async function reconcileHooks() {
  try {
    const res = await api('/hook-config', {
      method: 'POST',
      body: JSON.stringify({ reconcile: true })
    });
    if (res.ok) {
      const data = await res.json();
      await loadHooks();
      if (data.reconcile_reports) {
        let msg = data.reconcile_reports.map(r => `${r.agent}: ${r.action}`).join('\n');
        alert('Reconcile result:\n' + msg);
      }
    }
  } catch (e) { /* ignore */ }
}
