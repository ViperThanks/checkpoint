/* conversations.js — Conversations tab */

const CONV_LIST_LIMIT = 500;

/* Clear browser text selection to prevent blue residue on navigation */
function clearTextSelection() {
  var active = document.activeElement;
  var tag = active && active.tagName ? active.tagName.toLowerCase() : '';
  if (tag === 'textarea' || tag === 'input') return;
  var sel = window.getSelection && window.getSelection();
  if (sel && sel.rangeCount) sel.removeAllRanges();
}

/* ---------- Filter bar ---------- */
function renderConvFilterBar() {
  const bar = document.getElementById('conv-filter-bar');
  if (!bar) return;
  const agents = [
    { key: '', label: '全部' },
    { key: 'claude_code', label: 'Claude Code' },
    { key: 'kimi_code', label: 'Kimi Code' },
    { key: 'codex_cli', label: 'Codex CLI' },
  ];
  let html = '';
  agents.forEach(a => {
    const active = S.conv.agentFilter === a.key ? ' active' : '';
    html += `<button class="filter-chip${active}" data-agent="${esc(a.key)}" onclick="setConvAgent('${jsStr(a.key)}')">${esc(a.label)}</button>`;
  });
  bar.innerHTML = html;
}

/* ---------- List ---------- */
function loadConvList() {
  const list = document.getElementById('conv-list');
  if (!list) return;

  renderConvFilterBar();

  if (list.querySelectorAll('.conv-card, .skeleton-card').length === 0) {
    list.innerHTML = '';
    for (let i = 0; i < 4; i++) list.innerHTML += SKELETON_HTML;
  }

  S.conv.offset = 0;
  let qs = 'limit=' + CONV_LIST_LIMIT + '&offset=0';
  if (S.conv.agentFilter) qs += '&agent=' + encodeURIComponent(S.conv.agentFilter);
  api('/overview?' + qs).then(r => {
    if (r.error) return;
    S.conv.total = r.total || 0;
    const convs = r.conversations || [];
    renderConvList(convs);
    syncConvDetailSelection(convs);
    updateConvPager();
  });
}

function renderConvList(convs) {
  const list = document.getElementById('conv-list');
  list.querySelectorAll('.skeleton-card').forEach(el => el.remove());
  if (!convs.length) {
    list.innerHTML = emptyState('暂无会话', '使用 Claude Code / Kimi Code / Codex CLI 后会自动显示');
    return;
  }
  const groups = {};
  convs.forEach(c => {
    const p = c.project_path || '未分类';
    if (!groups[p]) groups[p] = [];
    groups[p].push(c);
  });
  // Sort groups by max last_seen_at desc
  const groupEntries = Object.entries(groups).sort((a, b) => {
    const selectedA = a[1].some(c => c.id === S.conv.detailCid);
    const selectedB = b[1].some(c => c.id === S.conv.detailCid);
    if (selectedA !== selectedB) return selectedA ? -1 : 1;
    const maxA = Math.max(...a[1].map(c => new Date(c.last_seen_at || 0).getTime()));
    const maxB = Math.max(...b[1].map(c => new Date(c.last_seen_at || 0).getTime()));
    return maxB - maxA;
  });
  let html = '';
  groupEntries.forEach(([path, listConvs]) => {
    listConvs.sort((a, b) => {
      if (a.id === S.conv.detailCid) return -1;
      if (b.id === S.conv.detailCid) return 1;
      return new Date(b.last_seen_at || 0).getTime() - new Date(a.last_seen_at || 0).getTime();
    });
    const agent = S.conv.agentFilter || (listConvs[0] && listConvs[0].agent) || 'claude_code';
    html += '<div class="conv-project-head">' +
      '<span class="conv-project-title">' + esc(projectBasename(path)) + '</span>' +
      '<button class="conv-project-new" onclick="event.stopPropagation();startNewConversationFromProject(\'' + jsStr(agent) + '\',\'' + jsStr(path) + '\')" title="在 ' + esc(projectBasename(path)) + ' 新建会话" aria-label="新建会话">' +
        '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 5v14"/><path d="M5 12h14"/></svg>' +
      '</button>' +
      '</div>';
    listConvs.forEach(c => html += buildConvCardHTML(c));
  });
  list.innerHTML = html;
}

function syncConvDetailSelection(convs) {
  const desktop = window.innerWidth >= 768;
  const detail = document.getElementById('conv-detail-panel');
  const header = document.getElementById('conv-detail-header');
  const content = document.getElementById('conv-sub-content');
  if (!convs.length) {
    S.conv.detailCid = null;
    S.conv.current = null;
    if (header) header.innerHTML = '';
    if (content) {
      content.classList.remove('chat-mode');
      content.innerHTML = emptyState('暂无会话', '当前筛选条件下没有可展示的会话');
    }
    return;
  }
  if (S.conv.detailCid && convs.some(c => c.id === S.conv.detailCid)) {
    document.querySelectorAll('.conv-card').forEach(el => {
      el.classList.toggle('selected', el.dataset.cid === S.conv.detailCid);
    });
    return;
  }
  if (!desktop) {
    if (header) header.innerHTML = '';
    if (content && !S.conv.detailCid) {
      content.classList.remove('chat-mode');
      content.innerHTML = emptyState('选择会话', '点开左侧会话查看聊天和工具活动');
    }
    return;
  }
  if (detail && !S.conv.detailCid) {
    openConvDetail(convs[0].id);
  }
}

function buildConvCardHTML(c) {
  const src = (c.title_source && c.title_source !== 'fallback')
    ? ' <span class="title-source-label">' + esc(c.title_source) + '</span>'
    : '';
  const pendingAsk = c.pending_ask_count || c.ask_count || 0;
  const denyN = c.deny_count || 0;
  let counts = '';
  if (pendingAsk > 0) counts += '<span class="count-badge count-ask">' + pendingAsk + ' 待审批</span>';
  if (denyN > 0) counts += '<span class="count-badge count-deny">' + denyN + ' 拒绝</span>';
  const resumeBadge = c.can_resume === false
    ? '<span class="resume-badge resume-view">仅查看</span>'
    : '<span class="resume-badge resume-ok">可继续</span>';
  // Runtime health badge
  var healthBadge = '';
  if (c.runtime_health && c.runtime_health.status !== 'unknown') {
    var hs = c.runtime_health.status;
    if (hs === 'critical') healthBadge = ' <span class="health-badge health-critical">环境漂移</span>';
    else if (hs === 'warning') healthBadge = ' <span class="health-badge health-warning">环境变更</span>';
  }
  const runningBadge = (S.home.activeJobConvId && S.home.activeJobConvId === c.conversation_id)
    ? ' <span class="count-badge count-ask"><span class="run-spinner" style="width:10px;height:10px;display:inline-block;vertical-align:middle;margin-right:3px"></span>运行中</span>'
    : '';
  const tokens = (c.token_count_label && c.token_count_label !== '0')
    ? '<span style="color:var(--dim);font-size:.68rem">' + esc(c.token_count_label) + ' tokens</span>'
    : '';
  const size = (c.file_size_label && c.file_size_label !== '0 B')
    ? '<span style="color:var(--dim);font-size:.68rem">' + esc(c.file_size_label) + '</span>'
    : '';
  return '<div class="conv-card" data-cid="' + esc(c.id) + '" onclick="openConvDetail(\'' + jsStr(c.id) + '\')">' +
    '<div class="conv-card-top"><span class="conv-card-title">' + esc(c.title || '未命名') + src + '</span>' +
    '<span class="badge-agent">' + esc(AGENTS[c.agent] || c.agent) + '</span>' + resumeBadge + runningBadge + healthBadge + '</div>' +
    '<div class="card-meta"><span>' + esc(projectBasename(c.project_path)) + '</span>' +
    '<span class="conv-id-short" title="' + esc(c.conversation_id || '') + '">' + shortId(c.conversation_id) + '</span>' +
    copyButton(c.conversation_id || '', 'ID') +
    '<span>活跃 ' + ago(c.last_seen_at) + '</span>' +
    tokens + size + '</div>' +
    '<div class="conv-card-stats">' + counts + '</div></div>';
}

function updateConvPager() {
  const info = document.getElementById('conv-pager-info');
  const btnPrev = document.getElementById('conv-btn-prev');
  const btnNext = document.getElementById('conv-btn-next');
  if (btnPrev) btnPrev.classList.add('hidden');
  if (btnNext) btnNext.classList.add('hidden');
  if (info) {
    const shown = Math.min(S.conv.total || 0, CONV_LIST_LIMIT);
    info.textContent = (S.conv.total || 0) > shown ? '显示 ' + shown + ' / ' + S.conv.total : shown + ' 条';
  }
}

function convPagePrev() {
  loadConvList();
}

function convPageNext() {
  loadConvList();
}

function setConvAgent(agent) {
  S.conv.agentFilter = agent;
  S.conv.offset = 0;
  document.querySelectorAll('#conv-filter-bar .filter-chip').forEach(el => {
    el.classList.toggle('active', el.dataset.agent === agent);
  });
  loadConvList();
}

/* ---------- Detail ---------- */
function openConvDetail(cid) {
  clearTextSelection();
  S.conv.detailCid = cid;
  S.conv.subTab = 'chat';
  stopActivityPoll();
  S.conv.messagesLoaded = false;
  document.querySelectorAll('.conv-card').forEach(el => el.classList.toggle('selected', el.dataset.cid === cid));
  const cdp = document.getElementById('conv-detail-panel');
  if (cdp) cdp.classList.add('mobile-open');
  loadConvDetail();
}

function closeConvDetail() {
  S.conv.detailCid = null;
  S.conv.current = null;
  document.querySelectorAll('.conv-card').forEach(el => el.classList.remove('selected'));
  S.conv.subTab = 'chat';
  stopChatPoll();
  stopActivityPoll();
  if (_chatObserver) { _chatObserver.disconnect(); _chatObserver = null; }
  closeConvRunDialog();
  const cdp = document.getElementById('conv-detail-panel');
  if (cdp) cdp.classList.remove('mobile-open');
  loadConvList();
}

function loadConvDetail() {
  const cid = S.conv.detailCid;
  if (!cid) return;

  api('/conversations/' + cid).then(r => {
    if (r.error) { closeConvDetail(); return; }
    const c = r;
    S.conv.current = c;

    const header = document.getElementById('conv-detail-header');
    if (header) {
      const isMobile = window.innerWidth < 768;
      // Runtime health banner
      var healthBanner = '';
      if (c.runtime_health && c.runtime_health.status === 'critical') {
        healthBanner = '<div class="health-banner health-banner-critical">⚠ 运行环境已漂移 — <button onclick="checkRuntimeHealth(\'' + jsStr(cid) + '\')">检查详情</button></div>';
      } else if (c.runtime_health && c.runtime_health.status === 'warning') {
        healthBanner = '<div class="health-banner health-banner-warning">运行环境有变更 — <button onclick="checkRuntimeHealth(\'' + jsStr(cid) + '\')">检查详情</button></div>';
      }
      // Runtime identity chips
      var identityChips = '';
      if (c.runtime_health && c.runtime_health.status !== 'unknown') {
        var rh = c.runtime_health;
        identityChips = '<div class="identity-chips">';
        if (rh.model_id) identityChips += '<span class="id-chip">模型: ' + esc(rh.model_id) + '</span>';
        if (rh.runtime_profile) identityChips += '<span class="id-chip">Profile: ' + esc(rh.runtime_profile) + '</span>';
        if (rh.permission_mode && rh.permission_mode !== 'unknown') identityChips += '<span class="id-chip">权限: ' + esc(rh.permission_mode === 'bypassPermissions' ? 'bypass' : rh.permission_mode) + '</span>';
        identityChips += '</div>';
      }
      header.innerHTML =
        (isMobile ? '<button class="btn-back" onclick="closeConvDetail()"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><line x1="19" y1="12" x2="5" y2="12"/><polyline points="12 19 5 12 12 5"/></svg>返回</button>' : '') +
        '<div class="conv-detail-title">' + esc(c.title || '未命名') + '</div>' +
        '<button class="conv-new-chat-btn" onclick="startNewConversationFromCurrent()">新建会话</button>' +
        '<div class="conv-detail-meta">' +
        '<span class="badge-agent">' + esc(AGENTS[c.agent] || c.agent) + '</span>' +
        (c.can_resume === false ? '<span class="resume-badge resume-view">仅查看</span>' : '<span class="resume-badge resume-ok">可继续</span>') +
        '<span>' + esc(projectBasename(c.project_path)) + '</span>' +
        copyButton(c.conversation_id || '', '对话ID') +
        '</div>' +
        identityChips +
        healthBanner;
    }

    const tabs = document.querySelector('.conv-sub-tabs');
    if (tabs) {
      tabs.innerHTML =
        '<button id="sub-chat" class="sub-tab' + (S.conv.subTab === 'chat' ? ' active' : '') + '" onclick="switchSubTab(\'chat\')">聊天</button>' +
        '<button id="sub-tools" class="sub-tab' + (S.conv.subTab === 'tools' ? ' active' : '') + '" onclick="switchSubTab(\'tools\')">工具</button>';
    }

    loadConvSubContent();
  });
}

function submitConvRun() {
  var c = S.conv.current;
  if (!c) return;
  if (c.can_resume === false) {
    startNewConversationFromCurrent();
    return;
  }
  var ta = document.getElementById('conv-run-input');
  var prompt = ta ? ta.value.trim() : '';
  if (!prompt) { toast('请输入提示词'); return; }

  var body = buildContinueJobBody(c.agent, c.project_path, c.conversation_id, prompt);
  // Track active job for running indicator on cards
  S.home.activeJobConvId = c.conversation_id || null;
  const pending = appendConvPendingJob(prompt, c, { newConversation: false });
  if (ta) ta.value = '';
  api('/jobs', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  }).then(function (r) {
    if (r.error) {
      // Runtime drift or cost blocked — show confirmation dialog
      if (r.runtime_health && r.runtime_health.status === 'critical') {
        showDriftConfirmDialog(r, body, pending, c);
        return;
      }
      if (r.cost_stats) {
        showCostConfirmDialog(r, body, pending, c);
        return;
      }
      markConvPendingFailed(pending.assistantId, '提交失败: ' + r.error);
      toast('提交失败: ' + r.error);
      return;
    }
    bindConvPendingJob(r.job_id, pending.assistantId, c.agent, false);
  }).catch(function () {
    markConvPendingFailed(pending.assistantId, '提交失败');
    toast('提交失败');
  });
}

function chatInputBarHtml() {
  const c = S.conv.current;
  if (c && c.can_resume === false) {
    return '<div class="chat-input-bar chat-input-viewonly">' +
      '<div class="resume-note">' + esc(c.resume_note || '这个会话只能查看历史，请新建会话继续。') + '</div>' +
      '<textarea id="conv-run-input" class="textarea" placeholder="输入提示词开启一个新的 ' + esc(AGENTS[c.agent] || c.agent) + ' 会话…" rows="2" onkeydown="if(event.key===\'Enter\'&&!event.shiftKey){event.preventDefault();startNewConversationFromCurrent();}"></textarea>' +
      '<button class="btn btn-primary" onclick="startNewConversationFromCurrent()">新建会话</button>' +
      '</div>';
  }
  return '<div class="chat-input-bar">' +
    '<textarea id="conv-run-input" class="textarea" placeholder="输入提示词继续对话…" rows="2" onkeydown="if(event.key===\'Enter\'&&!event.shiftKey){event.preventDefault();submitConvRun();}"></textarea>' +
    '<button class="btn btn-primary" onclick="submitConvRun()">发送</button>' +
    '</div>';
}

function startNewConversationFromCurrent() {
  var c = S.conv.current;
  if (!c) return;
  var ta = document.getElementById('conv-run-input');
  var prompt = ta ? ta.value.trim() : '';
  if (prompt) {
    submitNewConversationJob(c, prompt, ta);
    return;
  }
  openNewConversationDialog(c.agent, c.project_path || '');
}

function startNewConversationFromProject(agent, projectPath) {
  openNewConversationDialog(agent, projectPath === '未分类' ? '' : projectPath);
}

function submitNewConversationJob(c, prompt, ta) {
  var body = buildNewJobBody(c.agent, c.project_path, prompt);
  const pending = appendConvPendingJob(prompt, c, { newConversation: true });
  if (ta) ta.value = '';
  api('/jobs', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  }).then(function (r) {
    if (r.error) {
      markConvPendingFailed(pending.assistantId, '新建会话失败: ' + r.error);
      toast('新建会话失败: ' + r.error);
      return;
    }
    bindConvPendingJob(r.job_id, pending.assistantId, c.agent, true);
  }).catch(function () {
    markConvPendingFailed(pending.assistantId, '新建会话失败');
    toast('新建会话失败');
  });
}

function ensureNewConversationDialog() {
  let dlg = document.getElementById('conv-new-dialog');
  if (dlg) return dlg;
  const host = document.getElementById('conv-view') || document.body;
  const wrap = document.createElement('div');
  wrap.id = 'conv-new-dialog';
  wrap.className = 'conv-new-dialog hidden';
  wrap.innerHTML =
    '<div class="conv-new-backdrop" onclick="closeNewConversationDialog()"></div>' +
    '<div class="conv-new-card">' +
      '<div class="conv-new-head">' +
        '<div><div class="conv-new-title">新建会话</div><div class="conv-new-sub" id="conv-new-sub"></div></div>' +
        '<button class="conv-new-close" onclick="closeNewConversationDialog()" aria-label="关闭">×</button>' +
      '</div>' +
      '<div class="conv-new-grid">' +
        '<label><span>Provider</span><select class="select" id="conv-new-agent">' +
          '<option value="claude_code">Claude Code</option>' +
          '<option value="kimi_code">Kimi Code</option>' +
          '<option value="codex_cli">Codex CLI</option>' +
        '</select></label>' +
        '<label><span>Project</span><input class="input" id="conv-new-project" placeholder="项目目录"/></label>' +
      '</div>' +
      '<label class="conv-new-prompt-label"><span>Prompt</span><textarea class="textarea" id="conv-new-prompt" rows="5" placeholder="输入提示词，开启一个新的 conversation..."></textarea></label>' +
      '<div class="conv-new-status" id="conv-new-status"></div>' +
      '<div class="conv-new-actions">' +
        '<button class="btn btn-ghost" onclick="closeNewConversationDialog()">取消</button>' +
        '<button class="btn btn-primary" id="conv-new-submit" onclick="submitNewConversationDialog()">提交</button>' +
      '</div>' +
    '</div>';
  host.appendChild(wrap);
  return wrap;
}

function openNewConversationDialog(agent, projectPath) {
  const dlg = ensureNewConversationDialog();
  const a = document.getElementById('conv-new-agent');
  const p = document.getElementById('conv-new-project');
  const prompt = document.getElementById('conv-new-prompt');
  const sub = document.getElementById('conv-new-sub');
  const status = document.getElementById('conv-new-status');
  if (a) a.value = agent || S.conv.agentFilter || 'claude_code';
  if (p) p.value = projectPath || '';
  if (prompt) prompt.value = '';
  if (status) status.textContent = '';
  if (sub) sub.textContent = (AGENTS[(agent || '')] || agent || 'Agent') + (projectPath ? ' · ' + projectBasename(projectPath) : '');
  dlg.classList.remove('hidden');
  setTimeout(function () { if (prompt) prompt.focus(); }, 0);
}

function closeNewConversationDialog() {
  const dlg = document.getElementById('conv-new-dialog');
  if (dlg) dlg.classList.add('hidden');
}

function submitNewConversationDialog() {
  const agent = (document.getElementById('conv-new-agent') || {}).value || 'claude_code';
  const projectPath = (document.getElementById('conv-new-project') || {}).value || '';
  const promptEl = document.getElementById('conv-new-prompt');
  const prompt = promptEl ? promptEl.value.trim() : '';
  const status = document.getElementById('conv-new-status');
  const btn = document.getElementById('conv-new-submit');
  if (!prompt) {
    if (status) status.textContent = '请输入提示词';
    return;
  }
  const body = buildNewJobBody(agent, projectPath, prompt);
  if (btn) btn.disabled = true;
  if (status) status.textContent = '正在提交...';
  api('/jobs', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  }).then(function (r) {
    if (btn) btn.disabled = false;
    if (r.error) {
      if (status) status.textContent = '提交失败: ' + r.error;
      return;
    }
    if (status) status.textContent = '已提交新会话任务，完成后会出现在左侧列表';
    if (promptEl) promptEl.value = '';
    toast('新会话已提交');
    setTimeout(closeNewConversationDialog, 700);
    loadConvList();
  }).catch(function () {
    if (btn) btn.disabled = false;
    if (status) status.textContent = '提交失败';
  });
}

function appendConvPendingJob(prompt, conv, opts) {
  opts = opts || {};
  S.conv.subTab = 'chat';
  const content = document.getElementById('conv-sub-content');
  if (content && !content.classList.contains('chat-mode')) {
    content.classList.add('chat-mode');
    content.innerHTML = '<div class="chat-messages" id="chat-messages"></div>' + chatInputBarHtml();
  }
  let box = document.getElementById('chat-messages');
  if (!box && content) {
    content.innerHTML = '<div class="chat-messages" id="chat-messages"></div>' + chatInputBarHtml();
    box = document.getElementById('chat-messages');
  }
  const stamp = Date.now().toString(36);
  const userId = 'conv-pending-user-' + stamp;
  const assistantId = 'conv-pending-assistant-' + stamp;
  if (box) {
    box.insertAdjacentHTML('beforeend',
      '<div class="chat-row chat-row-user">' +
        '<div class="chat-msg chat-user chat-pending-user" id="' + esc(userId) + '">' +
          '<div class="chat-role-row"><span class="chat-role">你</span><span class="chat-time">刚刚</span></div>' +
          '<div class="chat-text md-render">' + renderMd(prompt) + '</div>' +
        '</div>' +
      '</div>' +
      '<div class="chat-row chat-row-assistant">' +
        '<div class="chat-msg chat-assistant chat-pending-assistant" id="' + esc(assistantId) + '">' +
          '<div class="chat-role-row"><span class="chat-role">' + esc(AGENTS[conv.agent] || conv.agent || 'Agent') + '</span><span class="chat-pending-dot"></span></div>' +
          '<div class="chat-text md-render">正在处理...</div>' +
          '<div class="chat-job-status"><span class="count-badge count-ask">运行中</span><span>代理提示词</span><button class="job-cancel conv-job-cancel hidden" type="button">取消</button></div>' +
        '</div>' +
      '</div>'
    );
    box.scrollTop = box.scrollHeight;
  }
  S.conv.chatTotal = (S.conv.chatTotal || 0) + 2;
  const node = document.getElementById(assistantId);
  if (node) node.dataset.newConversation = opts.newConversation ? '1' : '0';
  return { userId: userId, assistantId: assistantId };
}

function bindConvPendingJob(jobId, assistantId, provider, newConversation) {
  const node = document.getElementById(assistantId);
  if (!node) return;
  node.dataset.jobId = jobId;
  node.dataset.afterId = '0';
  node.dataset.output = '';
  node.dataset.provider = provider || '';
  node.dataset.newConversation = newConversation ? '1' : (node.dataset.newConversation || '0');
  const cancel = node.querySelector('.conv-job-cancel');
  if (cancel) {
    cancel.classList.remove('hidden');
    cancel.onclick = function () { cancelConvPendingJob(jobId, assistantId); };
  }
  pollConvPendingJob(jobId, assistantId);
}

function pollConvPendingJob(jobId, assistantId) {
  const node = document.getElementById(assistantId);
  if (!node) return;
  refreshConvPendingJob(jobId, assistantId);
  if (node.dataset.timer) clearInterval(Number(node.dataset.timer));
  const timer = setInterval(function () {
    if (!document.getElementById(assistantId)) {
      clearInterval(timer);
      return;
    }
    refreshConvPendingJob(jobId, assistantId);
  }, 3000);
  node.dataset.timer = String(timer);
}

function refreshConvPendingJob(jobId, assistantId) {
  const node = document.getElementById(assistantId);
  if (!node) return;
  const afterId = Number(node.dataset.afterId || '0');
  api('/jobs/' + jobId + '/logs/delta', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ after_id: afterId, limit: 50 })
  }).then(function (r) {
    if (!r.error) {
      node.dataset.afterId = String(r.next_after_id || afterId);
      updateConvPendingText(assistantId, r.logs || []);
    }
  });
  api('/jobs/' + jobId).then(function (job) {
    if (job.error) return;
    if (job.status === 'failed' || job.status === 'cancelled' || job.status === 'timeout') {
      finishConvPendingJob(assistantId, job.status, job.failure_reason || '任务失败');
    } else if (job.status === 'succeeded') {
      const text = (node.dataset.output || '').trim() || '任务已完成。';
      finishConvPendingJob(assistantId, 'succeeded', text);
      setTimeout(function () {
        loadConvList();
        const shouldOpenNew = node.dataset.newConversation === '1';
        if (shouldOpenNew && job.conversation_db_id) {
          openConvDetail(job.conversation_db_id);
        } else if (S.conv.detailCid && S.conv.subTab === 'chat') {
          loadChatMessages(S.conv.detailCid, { preserveScroll: !isChatNearBottom() });
        }
      }, 900);
    }
  });
}

function updateConvPendingText(assistantId, logs) {
  const node = document.getElementById(assistantId);
  if (!node) return;
  const text = logs.map(cleanConvJobLogChunk).filter(Boolean).join('\n').trim();
  if (!text) return;
  const current = node.dataset.output || '';
  let next = current ? current + '\n' + text : text;
  if (next.length > 12000) next = next.slice(-12000);
  node.dataset.output = next;
  const shouldFollow = isChatNearBottom();
  const body = node.querySelector('.chat-text');
  if (body) body.innerHTML = renderMd(next);
  const box = document.getElementById('chat-messages');
  if (box && shouldFollow) box.scrollTop = box.scrollHeight;
}

function finishConvPendingJob(assistantId, status, text) {
  const node = document.getElementById(assistantId);
  if (!node) return;
  if (node.dataset.timer) {
    clearInterval(Number(node.dataset.timer));
    delete node.dataset.timer;
  }
  node.classList.remove('chat-pending-assistant');
  node.classList.toggle('chat-job-failed', status !== 'succeeded');
  const body = node.querySelector('.chat-text');
  if (body) body.innerHTML = renderMd(text || (status === 'succeeded' ? '任务已完成。' : '任务失败'));
  const dot = node.querySelector('.chat-pending-dot');
  if (dot) dot.remove();
  const statusBar = node.querySelector('.chat-job-status');
  if (statusBar) {
    const ok = status === 'succeeded';
    statusBar.innerHTML = '<span class="count-badge ' + (ok ? 'count-allow' : 'count-deny') + '">' + (ok ? '已完成' : '失败') + '</span>';
  }
}

function markConvPendingFailed(assistantId, text) {
  finishConvPendingJob(assistantId, 'failed', text || '任务失败');
}

function cancelConvPendingJob(jobId, assistantId) {
  api('/jobs/' + jobId + '/cancel', { method: 'POST' }).then(function (r) {
    if (r.error) {
      toast('取消失败: ' + r.error);
      return;
    }
    finishConvPendingJob(assistantId, 'cancelled', '任务已取消');
  });
}

// cleanConvJobLogChunk is shared via cleanAgentLogChunk in app.js
var cleanConvJobLogChunk = cleanAgentLogChunk;

function switchSubTab(sub) {
  clearTextSelection();
  S.conv.subTab = sub;
  const chatBtn = document.getElementById('sub-chat');
  const toolsBtn = document.getElementById('sub-tools');
  if (chatBtn) chatBtn.classList.toggle('active', sub === 'chat');
  if (toolsBtn) toolsBtn.classList.toggle('active', sub === 'tools');
  if (sub !== 'tools') stopActivityPoll();
  loadConvSubContent();
}

function loadConvSubContent() {
  const cid = S.conv.detailCid;
  if (!cid) return;
  const el = document.getElementById('conv-sub-content');
  if (!el) return;
  el.classList.remove('chat-mode');
  el.innerHTML = '';
  for (let i = 0; i < 3; i++) el.innerHTML += SKELETON_HTML;
  if (S.conv.subTab === 'chat') loadChatMessages(cid);
  else loadActivityTimeline(cid);
}

function isChatNearBottom(threshold) {
  const box = document.getElementById('chat-messages');
  if (!box) return true;
  const px = threshold || 120;
  return box.scrollHeight - box.scrollTop - box.clientHeight < px;
}

function loadChatMessages(cid, opts) {
  opts = opts || {};
  const oldBox = document.getElementById('chat-messages');
  const prevTop = oldBox ? oldBox.scrollTop : 0;
  api('/conversations/' + cid + '/messages?limit=30').then(r => {
    const el = document.getElementById('conv-sub-content');
    if (r.error) { el.innerHTML = emptyState('加载失败', r.error); return; }
    if (!r.messages || !r.messages.length) {
      el.classList.add('chat-mode');
      el.innerHTML = '<div class="chat-messages" id="chat-messages"></div>' + chatInputBarHtml();
      return;
    }
    S.conv.chatTotal = r.total || r.messages.length;
    S.conv.chatCursor = S.conv.chatTotal;
    S.conv.chatOffset = r.messages.length;
    renderChatMessages(r.messages, r.total, el, {
      preserveScroll: !!opts.preserveScroll,
      scrollTop: prevTop
    });
  });
}

/**
 * 使用 activity_segment.js 将消息分组并渲染。
 * 每个 tool run 独立显示 banner（自己的 duration/toolCount/phase）。
 * Run 用 runIdx/cardIdx 避免重复 DOM id。
 */
function buildActivityHtml(messages) {
  if (!messages || !messages.length) return '';
  var segments = buildSegments(messages);
  var groups = buildTurnGroups(segments, false);
  var tsState = { last: '' };
  var html = '';
  var runIdx = 0;
  var cardIdx = 0;
  var msgIdx = 0;

  for (var g = 0; g < groups.length; g++) {
    var group = groups[g];

    if (group.userSeg) {
      html += buildChatMessageHtml(group.userSeg.message, tsState, msgIdx++);
    }

    // 按 assistant 边界切分 tool runs
    var runs = buildToolRuns(group);
    var segCursor = 0; // tracks position in group.segments for interleaving

    for (var r = 0; r < runs.length; r++) {
      var run = runs[r];

      // 渲染 run 之前的 chat segments (assistant/user)
      for (; segCursor < group.segments.length; segCursor++) {
        var seg = group.segments[segCursor];
        if (seg.type === 'assistant') {
          html += buildChatMessageHtml(seg.message, tsState, msgIdx++);
        } else if (seg.type === 'user') {
          continue;
        } else {
          break; // hit a tool segment — stop, this belongs to the run
        }
      }

      // 渲染 tool run（使用 run 自身的 duration/toolCount/phase）
      html += '<div class="act-turn act-settled">';
      html += '<div class="act-turn-bar" onclick="toggleTurnBody(this)">';
      html += turnBannerLabel(run);
      html += '<span class="act-turn-chevron" id="act-turn-chevron-r' + runIdx + '">&#x25B8;</span>';
      html += '</div>';
      html += '<div class="act-turn-body" id="act-turn-body-r' + runIdx + '" style="display:none">';

      for (var s = 0; s < run.segments.length; s++) {
        html += renderSegmentCard(run.segments[s], 'c' + cardIdx);
        cardIdx++;
      }

      html += '</div></div>';
      runIdx++;

      // 跳过已渲染的 tool segments
      segCursor += run.segments.length;

      // 渲染 run 之后的 chat segments（直到下一个 run 或 group 结束）
      for (; segCursor < group.segments.length; segCursor++) {
        var after = group.segments[segCursor];
        if (after.type === 'assistant') {
          html += buildChatMessageHtml(after.message, tsState, msgIdx++);
        } else if (after.type === 'user') {
          continue;
        } else {
          break;
        }
      }
    }

    // 渲染 group 末尾剩余的 chat segments（没有 tool run 时的 assistant 消息）
    for (; segCursor < group.segments.length; segCursor++) {
      var tail = group.segments[segCursor];
      if (tail.type === 'assistant') {
        html += buildChatMessageHtml(tail.message, tsState, msgIdx++);
      }
    }
  }

  return html;
}

function extractThinking(text) {
  if (!text) return { thinking: '', content: text || '' };
  // Support <thinking>...</thinking> tags
  const match = text.match(/<thinking>([\s\S]*?)<\/thinking>/);
  if (match) {
    const thinking = match[1].trim();
    const content = text.replace(match[0], '').trim();
    return { thinking, content };
  }
  return { thinking: '', content: text };
}

function buildThinkingHtml(thinking, idx) {
  if (!thinking) return '';
  // Generate a brief summary: count lines or just show "已思考"
  const lines = thinking.split('\n').filter(l => l.trim()).length;
  const summary = lines > 1 ? '已思考 ' + lines + ' 步' : '思考过程';
  const id = 'thinking-' + idx;
  return '<div class="chat-thinking" id="' + id + '">' +
    '<div class="chat-thinking-bar" onclick="toggleThinking(\'' + id + '\')">' +
      '<span class="chat-thinking-label">' +
        '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 2a10 10 0 1 0 10 10A10 10 0 0 0 12 2zm0 16a1 1 0 1 1 1-1 1 1 0 0 1-1 1zm1-5h-2V7h2z"/></svg>' +
        'Thinking' +
      '</span>' +
      '<span class="chat-thinking-summary">' + esc(summary) + '</span>' +
      '<span class="chat-thinking-chevron">&#x25B8;</span>' +
    '</div>' +
    '<div class="chat-thinking-body">' + esc(thinking) + '</div>' +
  '</div>';
}

function toggleThinking(id) {
  const el = document.getElementById(id);
  if (el) el.classList.toggle('open');
}

function buildChatMessageHtml(m, tsState, idx) {
  const tsRaw = m.timestamp ? formatMsgTime(m.timestamp) : '';
  const showTs = tsRaw && (!tsState || tsRaw !== tsState.last);
  if (showTs && tsState) tsState.last = tsRaw;
  const ts = showTs ? '<span class="chat-time">' + esc(tsRaw) + '</span>' : '';
  if (m.role === 'user') {
    return '<div class="chat-row chat-row-user"><div class="chat-msg chat-user"><div class="chat-role-row"><span class="chat-role">你</span>' + ts + '</div><div class="chat-text md-render">' + renderMd(m.text) + '</div></div></div>';
  }
  if (m.role === 'assistant') {
    const t = m.thinking ? { thinking: m.thinking, content: m.text || '' } : extractThinking(m.text);
    const messageId = m.seq || idx || Math.random().toString(36).slice(2, 8);
    const thinkingHtml = buildThinkingHtml(t.thinking, messageId);
    const contentHtml = t.content ? '<div class="chat-text md-render">' + renderMd(t.content) + '</div>' : '';
    return '<div class="chat-row chat-row-assistant"><div class="chat-msg chat-assistant"><div class="chat-role-row"><span class="chat-role">助手</span>' + ts + '</div>' + thinkingHtml + contentHtml + '</div></div>';
  }
  if (m.role === 'tool_summary') {
    const hasFull = m.tool_input_full && m.tool_input_full !== m.tool_input_preview;
    return '<div class="chat-row chat-row-tool"><div class="chat-msg chat-tool"><div class="chat-tool-line">' +
      '<span class="chat-tool-name">' + esc(m.tool_name || 'tool') + '</span>' +
      (m.tool_input_preview ? '<span class="chat-tool-summary"' +
        (hasFull ? ' data-full="' + esc(m.tool_input_full) + '" data-preview="' + esc(m.tool_input_preview) + '" onclick="toggleToolSummary(this)" title="点击展开"' : '') +
        '>' + esc(m.tool_input_preview) + '</span>' : '') +
      ts +
      '</div></div></div>';
  }
  return '';
}

function renderChatMessages(messages, total, el, opts) {
  opts = opts || {};
  if (!el) el = document.getElementById('conv-sub-content');
  if (!el) return;
  el.classList.add('chat-mode');
  clearTextSelection();
  let msgsHtml = '<div class="chat-scroll-sentinel" id="chat-scroll-sentinel"></div><div id="chat-loading-indicator" class="chat-loading"><div class="chat-loading-spinner"></div><span>加载中...</span></div>';
  msgsHtml += buildActivityHtml(messages);
  el.innerHTML = '<div class="chat-messages" id="chat-messages">' + msgsHtml + '</div>' + chatInputBarHtml();
  const msgsEl = document.getElementById('chat-messages');
  if (msgsEl) {
    msgsEl.scrollTop = opts.preserveScroll ? (opts.scrollTop || 0) : msgsEl.scrollHeight;
  }
  S.conv.messagesLoaded = true;
  setupChatScrollObserver(msgsEl);
  startChatPoll();
}

function toggleToolSummary(el) {
  if (el.dataset.full) {
    const isExpanded = el.textContent !== el.dataset.preview;
    el.textContent = isExpanded ? el.dataset.preview : el.dataset.full;
    el.style.whiteSpace = isExpanded ? '' : 'pre-wrap';
  }
}

let _chatPollTimer = null;

function startChatPoll() {
  stopChatPoll();
  _chatPollTimer = setInterval(function () {
    if (!S.conv.detailCid || S.conv.subTab !== 'chat') return;
    var cursor = S.conv.chatCursor || S.conv.chatTotal || 0;
    api('/conversations/' + S.conv.detailCid + '/messages/delta?after=' + cursor + '&limit=50').then(function (r) {
      if (r.error || !r.messages) return;
      S.conv.chatCursor = r.cursor || cursor;
      S.conv.chatTotal = r.total || S.conv.chatTotal || 0;
      if (r.messages.length) {
        appendChatMessages(r.messages);
      }
    });
  }, 15000);
}

function appendChatMessages(messages) {
  const msgsEl = document.getElementById('chat-messages');
  if (!msgsEl) return;
  clearTextSelection();
  const nearBottom = msgsEl.scrollHeight - msgsEl.scrollTop - msgsEl.clientHeight < 80;
  let html = buildActivityHtml(messages);
  msgsEl.insertAdjacentHTML('beforeend', html);
  if (nearBottom) msgsEl.scrollTop = msgsEl.scrollHeight;
}

function stopChatPoll() {
  if (_chatPollTimer) { clearInterval(_chatPollTimer); _chatPollTimer = null; }
}

let _chatObserver = null;
let _chatLoadingMore = false;
let _scrollTriggered = false;
let _scrollHandler = null;

function setupChatScrollObserver(msgsEl) {
  if (_chatObserver) { _chatObserver.disconnect(); _chatObserver = null; }
  if (_scrollHandler) { msgsEl.removeEventListener('scroll', _scrollHandler); _scrollHandler = null; }
  _scrollTriggered = false;
  if (S.conv.chatOffset >= S.conv.chatTotal) return;
  // IntersectionObserver as primary trigger
  const sentinel = document.getElementById('chat-scroll-sentinel');
  if (sentinel) {
    _chatObserver = new IntersectionObserver(entries => {
      if (entries[0].isIntersecting && !_chatLoadingMore && !_scrollTriggered) {
        _scrollTriggered = true;
        loadOlderMessages();
      }
    }, { root: msgsEl, threshold: 0 });
    _chatObserver.observe(sentinel);
  }
  // Scroll event as fallback (mobile WebKit sometimes ignores IO with explicit root)
  _scrollHandler = function() {
    if (_chatLoadingMore || _scrollTriggered) return;
    if (msgsEl.scrollTop < 50 && S.conv.chatOffset < S.conv.chatTotal) {
      _scrollTriggered = true;
      loadOlderMessages();
    }
  };
  msgsEl.addEventListener('scroll', _scrollHandler, { passive: true });
}

function _showChatLoading(show) {
  const el = document.getElementById('chat-loading-indicator');
  if (el) { if (show) el.classList.add('visible'); else el.classList.remove('visible'); }
}

function loadOlderMessages() {
  const cid = S.conv.detailCid;
  if (!cid || _chatLoadingMore) return;
  _chatLoadingMore = true;
  _showChatLoading(true);
  const msgsEl = document.getElementById('chat-messages');
  if (!msgsEl) { _chatLoadingMore = false; _showChatLoading(false); return; }
  const prevHeight = msgsEl.scrollHeight;
  api('/conversations/' + cid + '/messages?limit=30&offset=' + S.conv.chatOffset).then(r => {
    _chatLoadingMore = false;
    _showChatLoading(false);
    if (r.error || !r.messages || !r.messages.length) return;
    S.conv.chatOffset += r.messages.length;
    const currentTopTimeEl = msgsEl.querySelector('.chat-msg .chat-time');
    const currentTopTime = currentTopTimeEl ? currentTopTimeEl.textContent : '';
    let html = '<div class="chat-scroll-sentinel" id="chat-scroll-sentinel"></div>';
    let tsState = { last: '' };
    r.messages.forEach((m, idx) => {
      const tsRaw = m.timestamp ? formatMsgTime(m.timestamp) : '';
      const isBoundary = idx === r.messages.length - 1;
      const showTs = tsRaw && tsRaw !== tsState.last && !(isBoundary && tsRaw === currentTopTime);
      if (showTs) tsState.last = tsRaw;
      const ts = showTs ? '<span class="chat-time">' + esc(tsRaw) + '</span>' : '';
      if (m.role === 'user') {
        html += '<div class="chat-row chat-row-user"><div class="chat-msg chat-user"><div class="chat-role-row"><span class="chat-role">你</span>' + ts + '</div><div class="chat-text md-render">' + renderMd(m.text) + '</div></div></div>';
      } else if (m.role === 'assistant') {
        html += '<div class="chat-row chat-row-assistant"><div class="chat-msg chat-assistant"><div class="chat-role-row"><span class="chat-role">助手</span>' + ts + '</div><div class="chat-text md-render">' + renderMd(m.text) + '</div></div></div>';
      } else if (m.role === 'tool_summary') {
        const hasFull = m.tool_input_full && m.tool_input_full !== m.tool_input_preview;
        html += '<div class="chat-row chat-row-tool"><div class="chat-msg chat-tool"><div class="chat-tool-line">' +
          '<span class="chat-tool-name">' + esc(m.tool_name || 'tool') + '</span>' +
          (m.tool_input_preview ? '<span class="chat-tool-summary"' +
            (hasFull ? ' data-full="' + esc(m.tool_input_full) + '" data-preview="' + esc(m.tool_input_preview) + '" onclick="toggleToolSummary(this)" title="点击展开"' : '') +
            '>' + esc(m.tool_input_preview) + '</span>' : '') +
          ts +
          '</div></div></div>';
      }
    });
    const oldSentinel = document.getElementById('chat-scroll-sentinel');
    if (oldSentinel) oldSentinel.remove();
    msgsEl.insertAdjacentHTML('afterbegin', html);
    // Use rAF to ensure layout is settled before restoring scroll position
    requestAnimationFrame(function() {
      msgsEl.scrollTop = msgsEl.scrollHeight - prevHeight;
    });
    if (S.conv.chatOffset < S.conv.chatTotal) setupChatScrollObserver(msgsEl);
  }).catch(() => { _chatLoadingMore = false; _showChatLoading(false); });
}

function loadActivityTimeline(cid) {
  api('/conversations/' + cid + '/activity?limit=50').then(r => {
    const el = document.getElementById('conv-sub-content');
    el.classList.remove('chat-mode');
    if (r.error || !r.activities || !r.activities.length) {
      el.innerHTML = emptyState('暂无工具活动', '');
      return;
    }
    let html = '';
    r.activities.forEach(a => {
      let tools = '', acts = '';
      (a.tools_summary || []).forEach(t => {
        tools += '<span class="act-chip">' + esc(t.tool_name) + ' (' + t.count + ')</span>';
      });
      const as = a.action_summary || {};
      if ((as.allow || 0) > 0) acts += badge('allow', as.allow + ' 允许');
      if ((as.ask || 0) > 0) acts += badge('ask', as.ask + ' 待审批');
      if ((as.deny || 0) > 0) acts += badge('deny', as.deny + ' 拒绝');
      html += '<div class="act-card"><div class="act-header">' +
        '<span class="act-kind">' + esc(a.kind || 'tool_group') + '</span>' +
        '<span class="act-title">' + esc(a.title_or_prompt || '活动') + '</span>' +
        '<span class="act-time">' + ago(a.started_at) + '</span></div>' +
        (tools ? '<div class="act-tools">' + tools + '</div>' : '') +
        (acts ? '<div style="margin-top:6px;display:flex;gap:6px;flex-wrap:wrap">' + acts + '</div>' : '') +
        '</div>';
    });
    el.innerHTML = html;
    _activitySignature = activitySignature(r);
    startActivityPoll();
  });
}

let _activityPollTimer = null;
let _activitySignature = '';

function activitySignature(r) {
  var first = r.activities && r.activities[0] ? r.activities[0] : {};
  return String(r.total || 0) + '|' + String(first.ended_at || first.started_at || '') + '|' + JSON.stringify(first.tools_summary || []);
}

function startActivityPoll() {
  stopActivityPoll();
  _activityPollTimer = setInterval(function () {
    if (!S.conv.detailCid || S.conv.subTab !== 'tools') return;
    api('/conversations/' + S.conv.detailCid + '/activity?limit=1').then(function (r) {
      if (r.error) return;
      var sig = activitySignature(r);
      if (sig !== _activitySignature) {
        _activitySignature = sig;
        loadActivityTimeline(S.conv.detailCid);
      }
    });
  }, 15000);
}

function stopActivityPoll() {
  if (_activityPollTimer) { clearInterval(_activityPollTimer); _activityPollTimer = null; }
}

/* ---------- Runtime Health ---------- */

function checkRuntimeHealth(cid) {
  api('/conversations/' + cid + '/runtime-check').then(function(r) {
    if (r.error) { toast('检查失败: ' + r.error); return; }
    var msg = '存储: ' + JSON.stringify(r.stored_identity) + '\n当前: ' + JSON.stringify(r.current_identity);
    if (r.health && r.health.warnings && r.health.warnings.length) {
      msg += '\n\n不匹配:\n' + r.health.warnings.map(function(w) {
        return w.field + ': ' + w.recorded + ' → ' + w.current;
      }).join('\n');
    } else {
      msg += '\n\n环境一致，无漂移。';
    }
    alert(msg);
  });
}

function _resubmitForceConfirm(originalBody, pending, c) {
  originalBody.force_confirm = true;
  api('/jobs', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(originalBody)
  }).then(function (r) {
    if (r.error) {
      markConvPendingFailed(pending.assistantId, '提交失败: ' + r.error);
      toast('提交失败: ' + r.error);
      return;
    }
    bindConvPendingJob(r.job_id, pending.assistantId, c.agent, false);
  }).catch(function () {
    markConvPendingFailed(pending.assistantId, '提交失败');
    toast('提交失败');
  });
}

function showDriftConfirmDialog(driftResp, originalBody, pending, c) {
  var warnings = (driftResp.runtime_health && driftResp.runtime_health.warnings) || [];
  var details = warnings.map(function(w) {
    return w.field + ': ' + w.recorded + ' \u2192 ' + w.current;
  }).join('\n');
  var msg = '运行环境发生漂移:\n' + details + '\n\n强制继续可能导致不可预期的行为。';
  if (!confirm(msg + '\n\n确认强制继续？')) {
    markConvPendingFailed(pending.assistantId, '已取消');
    toast('已取消');
    return;
  }
  _resubmitForceConfirm(originalBody, pending, c);
}

function showCostConfirmDialog(costResp, originalBody, pending, c) {
  var s = costResp.cost_stats || {};
  var tokens = Math.round((s.token_count || 0) / 1000);
  var sizeMB = Math.round((s.file_size_bytes || 0) / (1024 * 1024));
  var msg = '此会话已消耗较多资源:\n' +
    '  估算 token: ~' + tokens + 'k\n' +
    '  对话记录: ~' + sizeMB + 'MB\n' +
    '\n继续会增加成本。';
  if (!confirm(msg + '\n\n确认强制继续？')) {
    markConvPendingFailed(pending.assistantId, '已取消');
    toast('已取消');
    return;
  }
  _resubmitForceConfirm(originalBody, pending, c);
}

/* ---------- Sidebar resize ---------- */
(function() {
  var handle = document.getElementById('conv-resize-handle');
  var panel = document.getElementById('conv-list-panel');
  if (!handle || !panel) return;

  var STORAGE_KEY = 'cp_conv_list_w';
  var MIN = 200, MAX = 600;

  // Restore saved width
  var saved = parseInt(localStorage.getItem(STORAGE_KEY), 10);
  if (saved >= MIN && saved <= MAX) {
    panel.style.width = saved + 'px';
  }

  var dragging = false, startX = 0, startW = 0;

  handle.addEventListener('pointerdown', function(e) {
    dragging = true;
    startX = e.clientX;
    startW = panel.offsetWidth;
    handle.classList.add('active');
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    handle.setPointerCapture(e.pointerId);
    e.preventDefault();
  });

  handle.addEventListener('pointermove', function(e) {
    if (!dragging) return;
    var w = Math.min(MAX, Math.max(MIN, startW + (e.clientX - startX)));
    panel.style.width = w + 'px';
  });

  handle.addEventListener('pointerup', function(e) {
    if (!dragging) return;
    dragging = false;
    handle.classList.remove('active');
    document.body.style.cursor = '';
    document.body.style.userSelect = '';
    localStorage.setItem(STORAGE_KEY, panel.offsetWidth);
  });
})();
