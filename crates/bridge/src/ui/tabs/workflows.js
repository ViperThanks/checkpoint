/* workflows.js — Workflows tab: chain execution engine UI */

const WFS = {
  list: [],
  selected: null,
  pollTimer: null,
  createOpen: false,
  steps: [{ provider: 'claude_code', project_path: '', prompt: '', context_strategy: 'none' }]
};

window.WFS = WFS;

/* ---------- Layout ---------- */
function ensureWorkflowLayout() {
  const view = document.getElementById('workflows-view');
  if (!view || document.getElementById('wf-layout')) return;

  view.innerHTML =
    '<div id="wf-layout" style="display:flex;flex-direction:column;height:100%;overflow:hidden">' +
      '<div style="display:flex;align-items:center;justify-content:space-between;padding:12px 16px;border-bottom:1px solid var(--border);flex-shrink:0">' +
        '<h2 style="margin:0;font-size:1.1rem">工作流</h2>' +
        '<button class="btn btn-primary btn-sm" onclick="toggleWfCreate()">新建工作流</button>' +
      '</div>' +
      '<div id="wf-create-form" class="hidden" style="padding:16px;border-bottom:1px solid var(--border);background:var(--surface)"></div>' +
      '<div style="display:flex;flex:1;overflow:hidden">' +
        '<div id="wf-list-panel" style="width:320px;border-right:1px solid var(--border);overflow-y:auto;flex-shrink:0"></div>' +
        '<div id="wf-detail-panel" style="flex:1;overflow-y:auto;padding:16px"></div>' +
      '</div>' +
    '</div>';

  renderWfCreateForm();
}

/* ---------- Create Form ---------- */
function renderWfCreateForm() {
  const el = document.getElementById('wf-create-form');
  if (!el) return;

  let stepsHtml = '';
  WFS.steps.forEach((s, i) => {
    stepsHtml +=
      '<div style="display:flex;gap:8px;align-items:start;margin-bottom:8px;padding:8px;background:var(--bg);border-radius:6px">' +
        '<span style="min-width:24px;text-align:center;color:var(--dim);font-size:.8rem;padding-top:6px">' + (i + 1) + '</span>' +
        '<div style="flex:1;display:flex;flex-direction:column;gap:6px">' +
          '<div style="display:flex;gap:6px">' +
            '<select class="select wf-step-provider" data-idx="' + i + '" style="flex:1">' +
              '<option value="claude_code"' + (s.provider === 'claude_code' ? ' selected' : '') + '>Claude Code</option>' +
              '<option value="kimi_code"' + (s.provider === 'kimi_code' ? ' selected' : '') + '>Kimi Code</option>' +
              '<option value="codex_cli"' + (s.provider === 'codex_cli' ? ' selected' : '') + '>Codex CLI</option>' +
            '</select>' +
            '<select class="select wf-step-ctx" data-idx="' + i + '" style="width:140px">' +
              '<option value="none"' + (s.context_strategy === 'none' ? ' selected' : '') + '>无上下文</option>' +
              '<option value="last_50_lines"' + (s.context_strategy === 'last_50_lines' ? ' selected' : '') + '>最后 50 行</option>' +
              '<option value="last_100_lines"' + (s.context_strategy === 'last_100_lines' ? ' selected' : '') + '>最后 100 行</option>' +
              '<option value="full_log"' + (s.context_strategy === 'full_log' ? ' selected' : '') + '>完整日志</option>' +
            '</select>' +
          '</div>' +
          '<input class="input wf-step-project" data-idx="' + i + '" placeholder="项目路径（可选）" value="' + esc(s.project_path) + '">' +
          '<textarea class="textarea wf-step-prompt" data-idx="' + i + '" placeholder="步骤提示词..." style="min-height:60px">' + esc(s.prompt) + '</textarea>' +
        '</div>' +
        (WFS.steps.length > 1 ? '<button class="icon-btn" onclick="removeWfStep(' + i + ')" title="删除" style="margin-top:4px"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg></button>' : '') +
      '</div>';
  });

  el.innerHTML =
    '<div style="display:flex;flex-direction:column;gap:10px">' +
      '<input class="input" id="wf-name" placeholder="工作流名称">' +
      '<input class="input" id="wf-desc" placeholder="描述（可选）">' +
      '<div style="font-size:.8rem;color:var(--dim)">步骤（按顺序执行）</div>' +
      '<div id="wf-steps-list">' + stepsHtml + '</div>' +
      '<div style="display:flex;gap:8px">' +
        '<button class="btn btn-sm" onclick="addWfStep()">添加步骤</button>' +
        '<button class="btn btn-primary btn-sm" onclick="submitCreateWf()" style="margin-left:auto">创建</button>' +
        '<button class="btn btn-sm" onclick="toggleWfCreate()">取消</button>' +
      '</div>' +
    '</div>';
}

function toggleWfCreate() {
  WFS.createOpen = !WFS.createOpen;
  const el = document.getElementById('wf-create-form');
  if (el) el.classList.toggle('hidden', !WFS.createOpen);
  if (WFS.createOpen) renderWfCreateForm();
}

function addWfStep() {
  syncWfStepsFromDom();
  WFS.steps.push({ provider: 'claude_code', project_path: '', prompt: '', context_strategy: 'none' });
  renderWfCreateForm();
}

function removeWfStep(idx) {
  syncWfStepsFromDom();
  WFS.steps.splice(idx, 1);
  renderWfCreateForm();
}

function syncWfStepsFromDom() {
  document.querySelectorAll('.wf-step-provider').forEach(el => {
    const i = parseInt(el.dataset.idx);
    if (WFS.steps[i]) WFS.steps[i].provider = el.value;
  });
  document.querySelectorAll('.wf-step-ctx').forEach(el => {
    const i = parseInt(el.dataset.idx);
    if (WFS.steps[i]) WFS.steps[i].context_strategy = el.value;
  });
  document.querySelectorAll('.wf-step-project').forEach(el => {
    const i = parseInt(el.dataset.idx);
    if (WFS.steps[i]) WFS.steps[i].project_path = el.value;
  });
  document.querySelectorAll('.wf-step-prompt').forEach(el => {
    const i = parseInt(el.dataset.idx);
    if (WFS.steps[i]) WFS.steps[i].prompt = el.value;
  });
}

function submitCreateWf() {
  syncWfStepsFromDom();
  const name = (document.getElementById('wf-name') || {}).value || '';
  const desc = (document.getElementById('wf-desc') || {}).value || '';
  if (!name.trim()) { toast('请输入名称'); return; }
  if (WFS.steps.length === 0) { toast('至少需要一个步骤'); return; }
  for (const s of WFS.steps) {
    if (!s.prompt.trim()) { toast('步骤提示词不能为空'); return; }
  }

  api('/workflows', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name: name.trim(), description: desc.trim(), steps: WFS.steps })
  }).then(r => r.json()).then(data => {
    if (data.id) {
      toast('工作流已创建');
      WFS.createOpen = false;
      const el = document.getElementById('wf-create-form');
      if (el) el.classList.add('hidden');
      WFS.steps = [{ provider: 'claude_code', project_path: '', prompt: '', context_strategy: 'none' }];
      loadWorkflowList();
      selectWorkflow(data.id);
    } else {
      toast(data.error || '创建失败');
    }
  }).catch(e => toast('请求失败: ' + e));
}

/* ---------- List ---------- */
function loadWorkflowList() {
  api('/workflows?limit=50').then(r => r.json()).then(data => {
    WFS.list = data.workflows || [];
    renderWfList();
  }).catch(() => {});
}

function renderWfList() {
  const el = document.getElementById('wf-list-panel');
  if (!el) return;

  if (WFS.list.length === 0) {
    el.innerHTML = '<div style="padding:24px;text-align:center;color:var(--dim)">暂无工作流</div>';
    return;
  }

  el.innerHTML = WFS.list.map(wf => {
    const selected = WFS.selected && WFS.selected.id === wf.id;
    const badge = wfStatusBadge(wf.status);
    const counts = wf.step_counts || {};
    return '<div class="wf-card' + (selected ? ' wf-card-selected' : '') + '" onclick="selectWorkflow(\'' + wf.id + '\')" style="padding:12px 16px;border-bottom:1px solid var(--border);cursor:pointer' + (selected ? ';background:var(--surface)' : '') + '">' +
      '<div style="display:flex;align-items:center;gap:8px">' +
        '<span style="font-weight:500;font-size:.9rem">' + esc(wf.name) + '</span>' +
        badge +
      '</div>' +
      (wf.description ? '<div style="font-size:.78rem;color:var(--dim);margin-top:4px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">' + esc(wf.description) + '</div>' : '') +
      '<div style="font-size:.72rem;color:var(--dim);margin-top:4px">' +
        (counts.total || 0) + ' 步 · ' +
        '<span style="color:var(--green)">' + (counts.succeeded || 0) + ' 完成</span>' +
        (counts.failed ? ' · <span style="color:var(--red)">' + counts.failed + ' 失败</span>' : '') +
      '</div>' +
    '</div>';
  }).join('');
}

/* ---------- Detail ---------- */
function selectWorkflow(id) {
  api('/workflows/' + id).then(r => r.json()).then(data => {
    WFS.selected = data;
    renderWfList();
    renderWfDetail();
  }).catch(() => {});
}

function renderWfDetail() {
  const el = document.getElementById('wf-detail-panel');
  if (!el || !WFS.selected) {
    if (el) el.innerHTML = '<div style="padding:24px;text-align:center;color:var(--dim)">选择一个工作流</div>';
    return;
  }

  const wf = WFS.selected;
  const badge = wfStatusBadge(wf.status);
  const canRun = wf.status === 'draft' || wf.status === 'failed' || wf.status === 'cancelled';
  const canCancel = wf.status === 'running';

  let stepsHtml = (wf.steps || []).map((s, i) => {
    const stepBadge = stepStatusBadge(s.status);
    return '<div style="display:flex;gap:12px;padding:10px 0' + (i < wf.steps.length - 1 ? ';border-bottom:1px solid var(--border)' : '') + '">' +
      '<div style="min-width:32px;display:flex;flex-direction:column;align-items:center">' +
        '<div style="width:24px;height:24px;border-radius:50%;background:' + stepColor(s.status) + ';display:flex;align-items:center;justify-content:center;font-size:.7rem;color:#fff">' + (i + 1) + '</div>' +
        (i < wf.steps.length - 1 ? '<div style="width:2px;flex:1;background:var(--border);margin-top:4px"></div>' : '') +
      '</div>' +
      '<div style="flex:1">' +
        '<div style="display:flex;align-items:center;gap:8px">' +
          '<span style="font-size:.85rem;font-weight:500">' + esc(s.provider || 'unknown') + '</span>' +
          stepBadge +
          (s.context_strategy !== 'none' ? '<span style="font-size:.68rem;color:var(--dim);background:var(--surface);padding:1px 6px;border-radius:4px">' + s.context_strategy + '</span>' : '') +
        '</div>' +
        (s.project_path ? '<div style="font-size:.75rem;color:var(--dim);margin-top:2px">' + esc(s.project_path) + '</div>' : '') +
        '<div style="font-size:.8rem;margin-top:4px;white-space:pre-wrap;color:var(--text)">' + esc(s.prompt) + '</div>' +
        (s.job_id ? '<div style="font-size:.72rem;color:var(--dim);margin-top:4px">Job: ' + esc(s.job_id.substring(0, 8)) + '...</div>' : '') +
      '</div>' +
    '</div>';
  }).join('');

  el.innerHTML =
    '<div style="display:flex;align-items:center;gap:12px;margin-bottom:16px">' +
      '<h3 style="margin:0;font-size:1.1rem">' + esc(wf.name) + '</h3>' +
      badge +
      '<div style="margin-left:auto;display:flex;gap:8px">' +
        (canRun ? '<button class="btn btn-primary btn-sm" onclick="runWorkflow(\'' + wf.id + '\')">执行</button>' : '') +
        (canCancel ? '<button class="btn btn-sm" style="color:var(--red)" onclick="cancelWorkflow(\'' + wf.id + '\')">取消</button>' : '') +
      '</div>' +
    '</div>' +
    (wf.description ? '<p style="color:var(--dim);margin:0 0 16px;font-size:.85rem">' + esc(wf.description) + '</p>' : '') +
    '<div style="font-size:.78rem;color:var(--dim);margin-bottom:16px">创建于 ' + formatTime(wf.created_at) + '</div>' +
    '<div style="font-size:.85rem;font-weight:500;margin-bottom:12px">步骤</div>' +
    '<div>' + stepsHtml + '</div>';
}

/* ---------- Actions ---------- */
function runWorkflow(id) {
  api('/workflows/' + id + '/run', { method: 'POST' })
    .then(r => r.json())
    .then(data => {
      if (data.status === 'running') {
        toast('工作流已开始执行');
        selectWorkflow(id);
        startWfPolling();
      } else {
        toast(data.error || '执行失败');
      }
    })
    .catch(e => toast('请求失败: ' + e));
}

function cancelWorkflow(id) {
  api('/workflows/' + id + '/cancel', { method: 'POST' })
    .then(r => r.json())
    .then(data => {
      if (data.status === 'cancelled') {
        toast('工作流已取消');
        selectWorkflow(id);
        stopWfPolling();
      } else {
        toast(data.error || '取消失败');
      }
    })
    .catch(e => toast('请求失败: ' + e));
}

/* ---------- Polling ---------- */
function startWfPolling() {
  stopWfPolling();
  WFS.pollTimer = setInterval(() => {
    loadWorkflowList();
    if (WFS.selected) selectWorkflow(WFS.selected.id);
  }, 3000);
}

function stopWfPolling() {
  if (WFS.pollTimer) { clearInterval(WFS.pollTimer); WFS.pollTimer = null; }
}

/* ---------- Helpers ---------- */
function wfStatusBadge(status) {
  const colors = { draft: 'var(--dim)', running: 'var(--blue)', succeeded: 'var(--green)', failed: 'var(--red)', cancelled: 'var(--yellow)' };
  const labels = { draft: '草稿', running: '运行中', succeeded: '完成', failed: '失败', cancelled: '已取消' };
  return '<span style="font-size:.68rem;padding:2px 8px;border-radius:10px;background:' + (colors[status] || 'var(--dim)') + ';color:#fff">' + (labels[status] || status) + '</span>';
}

function stepStatusBadge(status) {
  const colors = { pending: 'var(--dim)', running: 'var(--blue)', succeeded: 'var(--green)', failed: 'var(--red)', cancelled: 'var(--yellow)', skipped: 'var(--dim)' };
  const labels = { pending: '待执行', running: '执行中', succeeded: '完成', failed: '失败', cancelled: '已取消', skipped: '跳过' };
  return '<span style="font-size:.65rem;padding:1px 6px;border-radius:8px;background:' + (colors[status] || 'var(--dim)') + ';color:#fff">' + (labels[status] || status) + '</span>';
}

function stepColor(status) {
  const colors = { pending: 'var(--dim)', running: 'var(--blue)', succeeded: 'var(--green)', failed: 'var(--red)', cancelled: 'var(--yellow)', skipped: 'var(--dim)' };
  return colors[status] || 'var(--dim)';
}

/* ---------- Tab Entry ---------- */
function loadWorkflows() {
  ensureWorkflowLayout();
  loadWorkflowList();
  // 如果有 running 的 workflow，启动 polling
  if (WFS.list.some(wf => wf.status === 'running')) {
    startWfPolling();
  }
}
