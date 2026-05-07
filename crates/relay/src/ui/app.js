// app.js — 手机端 Relay UI：状态管理、渲染、事件绑定
//
// 职责：UI 编排层。所有 job body 构造必须调用 job_body.js 中的
// buildNewJobBody / buildContinueJobBody，不允许在 submit 函数中
// 自行拼装 body object。
//
// 业务原语来源：job_body.js（由 mobile_ui.rs 在本文件之前注入）

// ============================================================
// State
// ============================================================
const S = {
  token: '',
  tab: 'home',
  health: { status: 'loading' }, // loading | online | offline
  overview: { conversations: [], total: 0, active_agents: [] },
  pending: { events: [], count: 0 },
  lastJob: null,
  runCtx: null, // { projects, recent_conversations, provider_availability }
  convDetail: null,
  convMessages: [],
  convMessageSeqs: new Set(),
  convCursor: 0,
  convTotal: 0,
  convDeltaTimer: null,
  convActiveJob: null,
  convLogCursor: null,
  convLogPollTimer: null,
  activeJob: null, // { id, status }
  logCursor: null,
  logPollTimer: null,
  homePollTimer: null,
  convDetailPollTimer: null,
  convAgentFilter: '', // '' = all, 'claude_code', 'kimi_code', 'codex_cli'
  beat: { rtt_ms: null, status: 'unknown', last_at: null, fail_count: 0 },
  beatTimer: null,
  beatInFlight: false,
  pendingRunPrefill: null, // { provider, projectPath } applied once in renderRun()
  prevTabBeforeDetail: 'convos',
};

// Markdown rendering — 来自 shared_ui/render.js（renderMd）
// API wrapper — 来自 shared_ui/api_client.js（api）

// ============================================================
// Auth
// ============================================================
function saveToken() {
  const input = document.getElementById('token-input');
  const btn = input.nextElementSibling;
  const token = input.value.trim();
  if (!token) return;
  if (btn) { btn.disabled = true; btn.textContent = '连接中...'; }
  S.token = token;
  localStorage.setItem('relay_pairing_token', token);
  document.getElementById('token-error').textContent = '';
  // Verify token before showing app
  fetch('/api/health', { headers: { 'Authorization': 'Bearer ' + token } })
    .then(res => {
      if (res.ok) {
        document.getElementById('token-form').classList.add('hidden');
        document.getElementById('app').classList.remove('hidden');
        init();
      } else if (res.status === 401 || res.status === 403) {
        throw { code: 'auth_failed', message: 'Token 无效或已过期' };
      } else {
        // Token might be valid but Mac offline — proceed optimistically
        document.getElementById('token-form').classList.add('hidden');
        document.getElementById('app').classList.remove('hidden');
        init();
      }
    })
    .catch(e => {
      if (e.code === 'auth_failed') {
        document.getElementById('token-error').textContent = e.message;
        localStorage.removeItem('relay_pairing_token');
        S.token = '';
      } else {
        // Network error — proceed optimistically
        document.getElementById('token-form').classList.add('hidden');
        document.getElementById('app').classList.remove('hidden');
        init();
      }
    })
    .finally(() => { if (btn) { btn.disabled = false; btn.textContent = '连接'; } });
}

function loadSavedToken() {
  const saved = localStorage.getItem('relay_pairing_token');
  if (saved) {
    S.token = saved;
    document.getElementById('token-form').classList.add('hidden');
    document.getElementById('app').classList.remove('hidden');
    init();
  }
}

function logout() {
  console.debug('[auth] logout — clearing token');
  localStorage.removeItem('relay_pairing_token');
  S.token = '';
  clearInterval(S.homePollTimer);
  clearInterval(S.convDetailPollTimer);
  clearInterval(S.beatTimer);
  clearTimeout(S.logPollTimer);
  S.homePollTimer = null;
  S.convDetailPollTimer = null;
  S.beatTimer = null;
  S.logPollTimer = null;
  document.getElementById('app').classList.add('hidden');
  document.getElementById('token-form').classList.remove('hidden');
  document.getElementById('token-input').value = '';
  document.getElementById('token-error').textContent = '';
}

// ============================================================
// Init
// ============================================================
function init() {
  setTheme(getTheme());
  checkHealth();
  loadHome();
  sendBeat();
  S.homePollTimer = setInterval(loadHomeSilent, 30000);
  S.beatTimer = setInterval(sendBeat, 15000);
}

// ============================================================
// Health check
// ============================================================
async function checkHealth() {
  const dot = document.getElementById('status-dot');
  dot.className = 'dot loading';
  try {
    await api('/api/health');
    S.health.status = 'online';
    dot.className = 'dot online';
    console.debug('[health] mac online');
  } catch (e) {
    S.health.status = 'offline';
    dot.className = 'dot offline';
    console.debug('[health] mac offline:', e.code);
    if (e.code === 'auth_failed') {
      showAuthError(e.message);
    }
  }
}

function showAuthError(msg) {
  document.getElementById('token-error').textContent = msg;
  logout();
}

// ============================================================
// Heartbeat & Latency Probe
// ============================================================
async function sendBeat() {
  if (S.beatInFlight) {
    console.debug('[beat] skipped: previous beat still in flight');
    return;
  }
  S.beatInFlight = true;
  const now = Date.now();
  const rid = crypto.randomUUID ? crypto.randomUUID() : now.toString(36) + Math.random().toString(36);
  try {
    const res = await api('/api/beat-from-mobile', {
      method: 'POST',
      body: JSON.stringify({
        request_id: rid,
        device_id: 'mobile-web',
        client_sent_at_ms: now,
      }),
    });
    const data = await res.json();
    const received = Date.now();
    const rtt = received - data.client_sent_at_ms;
    S.beat.rtt_ms = rtt;
    S.beat.last_at = received;
    S.beat.fail_count = 0;
    // Classify: <3s online, 3-10s slow, 10-30s unstable
    if (rtt < 3000) S.beat.status = 'online';
    else if (rtt < 10000) S.beat.status = 'slow';
    else S.beat.status = 'unstable';
    console.debug('[beat] rtt=%dms status=%s', rtt, S.beat.status);
  } catch (e) {
    S.beat.fail_count++;
    S.beat.status = S.beat.fail_count >= 3 ? 'offline' : S.beat.status;
    console.debug('[beat] failed (%d): %s', S.beat.fail_count, e.code);
  } finally {
    S.beatInFlight = false;
  }
  // Update header dot based on beat status (more reliable than health check)
  const dot = document.getElementById('status-dot');
  if (dot) {
    const cls = S.beat.status === 'online' ? 'online' : S.beat.status === 'slow' ? 'loading' : S.beat.status === 'unstable' ? 'loading' : 'offline';
    dot.className = 'dot ' + cls;
  }
  if (S.tab === 'home') updateBeatDisplay();
}

function updateBeatDisplay() {
  const el = document.getElementById('beat-info');
  if (!el) return;
  const b = S.beat;
  const rtt = b.rtt_ms !== null ? b.rtt_ms + 'ms' : '--';
  const statusLabel = {online: '良好', slow: '偏慢', unstable: '不稳定', offline: '离线', unknown: '检测中'}[b.status] || '检测中';
  const statusColor = {online: '#22c55e', slow: '#eab308', unstable: '#f97316', offline: '#ef4444', unknown: '#888'}[b.status] || '#888';
  el.innerHTML =
    '<span class="status-value" style="color:' + statusColor + '">' + statusLabel + '</span>' +
    '<span class="status-label" style="margin-left:8px">RTT ' + rtt + '</span>';
}

// ============================================================
// Tab switching
// ============================================================
function switchTab(tab) {
  document.body.classList.remove('conv-detail-open');
  clearTimeout(S.convLogPollTimer);
  clearTimeout(S.convDeltaTimer);
  S.convLogPollTimer = null;
  S.convDeltaTimer = null;
  S.tab = tab;
  document.querySelectorAll('.tab').forEach(t => t.classList.remove('active'));
  document.getElementById('tab-' + tab).classList.add('active');
  ['page-home', 'page-convos', 'page-run', 'page-settings', 'page-conv-detail'].forEach(id => {
    document.getElementById(id).classList.add('hidden');
  });
  document.getElementById('page-' + tab).classList.remove('hidden');
  if (tab === 'home') loadHome();
  else if (tab === 'convos') loadConversations();
  else if (tab === 'run') loadRunContext();
  else if (tab === 'settings') renderSettings();
}

// ============================================================
// Home tab
// ============================================================
async function loadHome() {
  renderHomeLoading();
  await Promise.allSettled([loadHealthStatus(), loadHomeOverview(), loadHomePending(), loadHomeLastJob()]);
  renderHome();
}

async function loadHomeSilent() {
  if (S.tab !== 'home') return;
  await Promise.allSettled([loadHealthStatus(), loadHomeOverview(), loadHomePending(), loadHomeLastJob()]);
  if (S.tab === 'home') renderHome();
}

async function loadHealthStatus() {
  try {
    await api('/api/health');
    S.health.status = 'online';
    document.getElementById('status-dot').className = 'dot online';
  } catch (e) {
    S.health.status = e.code === 'mac_offline' ? 'offline' : 'error';
    document.getElementById('status-dot').className = 'dot offline';
  }
}

async function loadHomeOverview() {
  try {
    const res = await api('/api/overview?limit=3');
    const data = await res.json();
    console.debug('[home] overview: %d conversations', (data.conversations || []).length);
    S.overview = data;
  } catch (e) {
    console.debug('[home] overview error:', e.code, e.message);
    S.overview = { conversations: null, total: 0, _error: e };
  }
}

async function loadHomePending() {
  try {
    const res = await api('/api/pending');
    const data = await res.json();
    S.pending = data;
    S.pending.count = (data.events || data || []).length;
    console.debug('[home] pending asks:', S.pending.count);
  } catch (e) {
    console.debug('[home] pending error:', e.code);
    S.pending = { events: [], count: 0, _error: e };
  }
}

async function loadHomeLastJob() {
  try {
    const res = await api('/api/jobs?limit=1');
    const data = await res.json();
    S.lastJob = (data.jobs || [])[0] || null;
    console.debug('[home] last job:', S.lastJob ? S.lastJob.status : 'none');
  } catch (e) {
    console.debug('[home] jobs error:', e.code);
    S.lastJob = null;
  }
}

async function decideEvent(eventId, action) {
  const el = document.getElementById('pending-' + eventId);
  const btns = el ? el.querySelectorAll('button') : [];
  btns.forEach(b => b.disabled = true);
  if (el) el.style.opacity = '0.5';
  console.debug('[decide] %s → %s', eventId, action);
  try {
    await api('/api/decide', {
      method: 'POST',
      body: JSON.stringify({ event_id: eventId, action: action })
    });
    console.debug('[decide] success:', eventId);
    if (el) el.remove();
    S.pending.events = (S.pending.events || []).filter(e => e.event_id !== eventId);
    S.pending.count = S.pending.events.length;
    const countEl = document.querySelector('.card-title span[style*="3b82f6"]');
    if (countEl) countEl.textContent = S.pending.count;
    if (S.pending.count === 0) renderHome();
  } catch (e) {
    console.debug('[decide] error:', e.code, e.message);
    if (el) el.style.opacity = '1';
    btns.forEach(b => b.disabled = false);
    alert('操作失败: ' + e.message);
  }
}

function renderHomeLoading() {
  const el = document.getElementById('page-home');
  el.innerHTML =
    '<h2>首页</h2>' +
    '<div class="home-grid">' +
    skelCard() + skelCard() + skelCard() + skelCard() +
    '</div>';
}

function renderHome() {
  const el = document.getElementById('page-home');
  const statusCard = renderHomeStatusCard();
  const pendingCard = renderHomePendingCard();
  const convosCard = renderHomeConvosCard();
  const jobCard = renderHomeJobCard();
  const critCount = (S.overview.conversations || []).filter(c => c.runtime_health && c.runtime_health.status === 'critical').length;
  console.debug('[renderHome] critical conversations:', critCount);
  el.innerHTML =
    '<h2>首页</h2>' +
    '<div class="home-grid">' +
    statusCard + pendingCard + convosCard + jobCard +
    '</div>';
}

function renderHomeStatusCard() {
  const b = S.beat;
  const s = b.status === 'unknown' ? S.health.status : b.status;
  const label = s === 'online' ? 'Mac 在线' : s === 'offline' ? 'Mac 离线' : s === 'slow' ? '连接偏慢' : s === 'unstable' ? '连接不稳定' : '检查中...';
  const dotClass = s === 'online' ? 'online' : s === 'offline' ? 'offline' : 'loading';
  const rtt = b.rtt_ms !== null ? b.rtt_ms + 'ms' : '--';
  return '<div class="card">' +
    '<div class="card-title">连接状态</div>' +
    '<div class="status-row">' +
    '<div class="dot ' + dotClass + '"></div>' +
    '<span class="status-value">' + label + '</span>' +
    '</div>' +
    '<div id="beat-info" class="status-row" style="margin-top:6px">' +
    '<span class="status-label">延迟 ' + rtt + '</span>' +
    '</div></div>';
}

function renderHomePendingCard() {
  const err = S.pending._error;
  const events = S.pending.events || [];
  const count = events.length;
  let body;
  if (err) {
    body = '<div style="color:#ef4444;font-size:13px">' + escHtml(err.message) + '</div>';
  } else if (count > 0) {
    body = events.slice(0, 3).map(ev => {
      var desc = renderApprovalReviewCompact(ev) || escHtml(ev.tool_name || '未知');
      return '<div class="pending-event" id="pending-' + ev.event_id + '">' +
        '<div class="pending-event-info">' + desc + '</div>' +
        '<div class="pending-event-actions">' +
        '<button class="sm" style="background:#22c55e" onclick="decideEvent(\'' + ev.event_id + '\',\'allow\')">允许</button>' +
        '<button class="sm danger" onclick="decideEvent(\'' + ev.event_id + '\',\'deny\')">拒绝</button>' +
        '</div></div>';
    }).join('') + (count > 3 ? '<div style="color:#666;font-size:12px;text-align:center;padding-top:8px">还有 ' + (count - 3) + ' 条</div>' : '');
  } else {
    body = '<div style="color:#666;font-size:13px">暂无待审批</div>';
  }
  return '<div class="card"><div class="card-title">待审批 <span style="color:#3b82f6">' + count + '</span></div>' + body + '</div>';
}

function renderHomeConvosCard() {
  const err = S.overview._error;
  const convos = S.overview.conversations;
  const alertHtml = runtimeAlertCard(convos || []);
  let body;
  if (err) {
    body = '<div style="color:#ef4444;font-size:13px">' + escHtml(err.message) + '</div>';
  } else if (!convos || convos.length === 0) {
    body = '<div style="color:#666;font-size:13px">' + emptyReason(S.overview) + '</div>';
  } else {
    body = convos.map(c => {
      const title = c.title || c.conversation_id || '未命名';
      const agent = agentLabel(c.agent);
      const time = relTime(c.last_seen_at || c.started_at);
      return '<div class="conv-item" role="button" tabindex="0" onclick="openConvDetail(\'' + jsStr(c.id) + '\')">' +
        '<div class="conv-header"><span class="conv-title">' + escHtml(title) + '</span>' +
        '<span class="conv-badges">' + runtimeHealthBadge(c.runtime_health) + '<span class="conv-tag">' + escHtml(agent) + '</span><span class="badge badge-gray">查看</span></span></div>' +
        '<div class="conv-meta"><span class="conv-tag">' + escHtml(time) + '</span></div>' +
        '</div>';
    }).join('');
  }
  return alertHtml + '<div class="card"><div class="card-title">最近会话</div>' + body + '</div>';
}

function renderHomeJobCard() {
  const job = S.lastJob;
  let body;
  if (!job) {
    body = '<div style="color:#666;font-size:13px">暂无任务</div>';
  } else {
    const statusBadge = jobBadge(job.status);
    const prompt = job.prompt || job.kind || '';
    body = '<div class="status-row">' + statusBadge +
      '<span class="status-value">' + escHtml(trunc(prompt, 60)) + '</span></div>';
  }
  return '<div class="card"><div class="card-title">最近任务</div>' + body + '</div>';
}

// ============================================================
// Conversations tab
// ============================================================
async function loadConversations() {
  const el = document.getElementById('page-convos');
  el.innerHTML = '<h2>会话</h2>' + skelCard() + skelCard() + skelCard();
  try {
    const agentParam = S.convAgentFilter ? '&agent=' + S.convAgentFilter : '';
    const res = await api('/api/overview?limit=50' + agentParam);
    const data = await res.json();
    S.overview = data;
    renderConversations(data);
  } catch (e) {
    console.debug('[convos] load error:', e.code, e.message);
    el.innerHTML = '<h2>会话</h2>' +
      '<div class="error-state">' +
      '<div class="empty-icon">&#9888;</div>' +
      '<div>' + escHtml(e.message) + '</div>' +
      '<button class="retry-btn" onclick="loadConversations()">重试</button></div>';
  }
}

function filterConvAgent(agent) {
  S.convAgentFilter = agent;
  loadConversations();
}

function renderConversations(data) {
  const el = document.getElementById('page-convos');
  const convos = data.conversations;
  const total = data.total || (convos ? convos.length : 0);
  const activeAgents = data.active_agents || [];

  // Build filter chips
  const chips = [{ key: '', label: '全部' }];
  activeAgents.forEach(a => chips.push({ key: a, label: agentLabel(a) }));
  // Add common agents if not in active list
  ['claude_code', 'kimi_code', 'codex_cli'].forEach(a => {
    if (!activeAgents.includes(a)) chips.push({ key: a, label: agentLabel(a) });
  });
  const chipHtml = '<div class="filter-chips">' + chips.map(c =>
    '<div class="chip' + (S.convAgentFilter === c.key ? ' active' : '') +
    '" onclick="filterConvAgent(\'' + c.key + '\')">' + c.label + '</div>'
  ).join('') + '</div>';

  if (!convos || convos.length === 0) {
    el.innerHTML = '<h2>会话</h2>' + chipHtml +
      '<div class="empty"><div class="empty-icon">&#128196;</div>' + emptyReason(data) + '</div>';
    return;
  }
  const alertHtml = runtimeAlertCard(convos);
  const list = convos.map(c => {
    const title = c.title || c.conversation_id || '未命名';
    const agent = agentLabel(c.agent);
    const project = shortProject(c.project_path);
    const time = relTime(c.last_seen_at || c.started_at);
    const pending = c.pending_ask_count || 0;
    const pendingTag = pending > 0 ? '<span class="badge badge-yellow">' + pending + ' 待审</span>' : '';
    const healthTag = runtimeHealthBadge(c.runtime_health);
    return '<div class="conv-item" role="button" tabindex="0" onclick="openConvDetail(\'' + jsStr(c.id) + '\')">' +
      '<div class="conv-header"><span class="conv-title">' + escHtml(title) + '</span><span class="conv-badges">' + pendingTag + healthTag + '<span class="badge badge-gray">查看</span></span></div>' +
      '<div class="conv-meta">' +
      '<span class="conv-tag">' + escHtml(agent) + '</span>' +
      (project ? '<span class="conv-tag">' + escHtml(project) + '</span>' : '') +
      '<span class="conv-tag">' + escHtml(time) + '</span>' +
      '</div></div>';
  }).join('');
  el.innerHTML = '<h2>会话 <span style="color:#666;font-weight:400;font-size:13px">' + total + '</span></h2>' +
    chipHtml + alertHtml + '<div class="card">' + list + '</div>';
}

// runtimeAlertCard, runtimeHealthBadge, runtimeHealthBanner — 来自 shared_ui/runtime_health.js

// ============================================================
// Conversation detail
// ============================================================
async function openConvDetail(id) {
  S.prevTabBeforeDetail = S.tab === 'conv-detail' ? S.prevTabBeforeDetail : S.tab;
  S.tab = 'conv-detail';
  document.body.classList.add('conv-detail-open');
  ['page-home', 'page-convos', 'page-run', 'page-settings'].forEach(pid =>
    document.getElementById(pid).classList.add('hidden'));
  document.getElementById('page-conv-detail').classList.remove('hidden');

  const el = document.getElementById('page-conv-detail');
  el.innerHTML = '<div class="chat-screen"><div class="chat-topbar">' +
    '<button class="btn-back icon" onclick="closeConvDetail()">&#8592;</button>' +
    '<div class="chat-title-wrap"><div class="chat-title">加载中...</div></div>' +
    '<button class="btn-new-chat" disabled>新建会话</button>' +
    '</div><div class="chat-body">' + skelCard() + skelCard() + '</div></div>';

  try {
    const res = await api('/api/conversations/' + id);
    const conv = await res.json();
    S.convDetail = conv;
    S.convMessages = [];
    S.convMessageSeqs = new Set();
    S.convCursor = 0;
    S.convTotal = 0;
    S.convActiveJob = null;
    S.convLogCursor = null;
    clearTimeout(S.convLogPollTimer);
    S.convLogPollTimer = null;
    console.debug('[conv-detail] loaded:', conv.title, conv.agent);

    renderConvDetailShell(conv);

    await loadConvMessages(id);
    scheduleConvDeltaPoll();
  } catch (e) {
    console.debug('[conv-detail] error:', e.code, e.message);
    el.innerHTML = '<div class="chat-screen"><div class="chat-topbar">' +
      '<button class="btn-back icon" onclick="closeConvDetail()">&#8592;</button>' +
      '<div class="chat-title-wrap"><div class="chat-title">会话</div></div>' +
      '<button class="btn-new-chat" disabled>新建会话</button>' +
      '</div><div class="error-state"><div class="empty-icon">&#9888;</div>' +
      '<div>' + escHtml(e.message) + '</div>' +
      '<button class="retry-btn" onclick="openConvDetail(\'' + id + '\')">重试</button></div></div>';
  }
}

function renderConvDetailShell(conv) {
  const el = document.getElementById('page-conv-detail');
  const title = conv.title || conv.conversation_id || '未命名';
  const canResume = conv.can_resume !== false;
  el.innerHTML =
    '<div class="chat-screen">' +
      '<div class="chat-topbar">' +
        '<button class="btn-back icon" onclick="closeConvDetail()" aria-label="返回会话列表">&#8592;</button>' +
        '<div class="chat-title-wrap">' +
          '<div class="chat-title">' + escHtml(title) + '</div>' +
          '<div class="chat-meta-line">' +
            '<span class="conv-tag agent">' + escHtml(agentLabel(conv.agent)) + '</span>' +
            (canResume ? '<span class="conv-tag ok">可继续</span>' : '<span class="conv-tag muted">只读</span>') +
            runtimeHealthBadge(conv.runtime_health) +
            (conv.project_path ? '<span class="chat-path">' + escHtml(shortProject(conv.project_path)) + '</span>' : '') +
          '</div>' +
        '</div>' +
        '<button class="btn-new-chat" onclick="openNewConversationFromDetail()">新建会话</button>' +
      '</div>' +
      runtimeHealthBanner(conv) +
      '<div class="chat-tabs"><button class="chat-tab active">聊天</button><button class="chat-tab muted" onclick="toast(\'工具视图稍后回来\')">工具</button></div>' +
      '<div class="chat-body">' +
        '<div id="conv-messages"></div>' +
      '</div>' +
      convComposerHtml(conv) +
    '</div>';
}

function convComposerHtml(conv) {
  const canResume = conv && conv.can_resume !== false;
  const ph = canResume ? '输入提示词继续对话...' : '这个会话暂不可继续，可新建会话...';
  const modeText = canResume
    ? '继续此会话 · ' + agentLabel(conv.agent) + ' · ' + shortId(conv.conversation_id)
    : '仅查看 · 请新建会话';
  return '<div class="chat-composer">' +
    '<div class="composer-mode">' + escHtml(modeText) + '</div>' +
    '<div class="composer-row">' +
      '<textarea id="conv-prompt-input" placeholder="' + escHtml(ph) + '" rows="2" ' + (canResume ? '' : 'disabled ') + 'onkeydown="if(event.key===\'Enter\'&&!event.shiftKey){event.preventDefault();submitConvPrompt();}"></textarea>' +
      '<button class="chat-send" id="conv-send-btn" onclick="submitConvPrompt()" ' + (canResume ? '' : 'disabled') + '>' + (canResume ? '继续' : '只读') + '</button>' +
    '</div>' +
    '</div>';
}

async function loadConvMessages(id, append) {
  const el = document.getElementById('conv-messages');
  if (!el) return;
  if (!append) el.innerHTML = '<div class="chat-loading">' + skelLines(5) + '</div>';
  try {
    const offset = append ? S.convMessages.length : 0;
    const res = await api('/api/conversations/' + id + '/messages?limit=30&offset=' + offset);
    const data = await res.json();
    const msgs = data.messages || [];
    if (append) {
      // Older pages should appear above the already loaded latest page.
      S.convMessages = mergeConvMessages(msgs, true).concat(S.convMessages);
    } else {
      S.convMessages = mergeConvMessages(msgs, false);
    }
    updateConvCursorFromMessages(msgs);
    if (typeof data.cursor === 'number') S.convCursor = Math.max(S.convCursor, data.cursor);
    const total = data.total || S.convMessages.length;
    S.convTotal = Math.max(S.convTotal, total);
    console.debug('[conv-messages] loaded %d messages (total %d)', msgs.length, total);
    if (S.convMessages.length === 0) {
      el.innerHTML = '<div class="chat-empty">暂无消息</div>';
      return;
    }
    renderConvMessageList(id, total, append);
  } catch (e) {
    console.debug('[conv-messages] error:', e.code);
    if (!append) {
      el.innerHTML = '<div class="chat-error">' +
        escHtml(e.message) + '</div>' +
        '<div style="text-align:center"><button class="retry-btn" onclick="loadConvMessages(\'' + id + '\')">重试</button></div>';
    }
  }
}

function mergeConvMessages(msgs, olderPage) {
  const out = [];
  msgs.forEach(m => {
    const key = m.seq !== undefined && m.seq !== null
      ? 'seq:' + m.seq
      : 'msg:' + (m.role || '') + ':' + (m.timestamp || '') + ':' + (m.text || m.tool_input_preview || '');
    if (S.convMessageSeqs.has(key)) return;
    S.convMessageSeqs.add(key);
    out.push(m);
  });
  return out;
}

function updateConvCursorFromMessages(msgs) {
  (msgs || []).forEach(m => {
    if (typeof m.seq === 'number') S.convCursor = Math.max(S.convCursor, m.seq);
  });
}

function normChatText(s) {
  return String(s || '').replace(/\s+/g, ' ').trim();
}

function msgText(m) {
  return m ? (m.text || m.content || m.tool_input_preview || '') : '';
}

function isLocalMsg(m) {
  return !!(m && m._local_id);
}

function isPendingAssistantPlaceholder(m) {
  if (!isLocalMsg(m) || m.role !== 'assistant') return false;
  if (m.pending) return true;
  const text = normChatText(msgText(m));
  return text === '排队中...' || text === '已提交，等待 Mac 返回输出...';
}

function reconcileLocalOptimisticMessages(fresh) {
  if (!fresh || !fresh.length) return;
  fresh.forEach(serverMsg => {
    if (isLocalMsg(serverMsg)) return;
    const role = serverMsg.role || '';
    const text = normChatText(msgText(serverMsg));
    if (!text) return;

    if (role === 'user') {
      const idx = S.convMessages.findIndex(m =>
        isLocalMsg(m) && m.role === 'user' && normChatText(msgText(m)) === text
      );
      if (idx >= 0) S.convMessages.splice(idx, 1);
      return;
    }

    if (role === 'assistant') {
      const activeAssistantId = S.convActiveJob && S.convActiveJob.assistantId;
      const idx = S.convMessages.findIndex(m =>
        (activeAssistantId && m._local_id === activeAssistantId) || isPendingAssistantPlaceholder(m)
      );
      if (idx >= 0) S.convMessages.splice(idx, 1);
    }
  });
}

function scheduleConvDeltaPoll() {
  clearTimeout(S.convDeltaTimer);
  if (!S.convDetail) return;
  S.convDeltaTimer = setTimeout(pollConvDelta, S.convActiveJob ? 2000 : 5000);
}

async function pollConvDelta() {
  const conv = S.convDetail;
  if (!conv) return;
  try {
    const body = document.querySelector('#page-conv-detail .chat-body');
    const shouldFollow = isScrollNearBottom(body);
    const res = await api('/api/conversations/' + conv.id + '/messages/delta?after=' + S.convCursor + '&limit=50');
    const data = await res.json();
    const msgs = data.messages || [];
    if (msgs.length) {
      const fresh = mergeConvMessages(msgs, false);
      if (fresh.length) {
        reconcileLocalOptimisticMessages(fresh);
        S.convMessages = S.convMessages.concat(fresh);
        renderConvMessageList(conv.id, Math.max(data.total || 0, S.convMessages.length), !shouldFollow);
      }
    }
    if (typeof data.cursor === 'number') S.convCursor = Math.max(S.convCursor, data.cursor);
    else updateConvCursorFromMessages(msgs);
  } catch (e) {
    console.debug('[conv-delta] error:', e.code);
  }
  scheduleConvDeltaPoll();
}

let _convLoadingOlder = false;
let _convScrollHandler = null;

function isScrollNearBottom(el, threshold) {
  if (!el) return true;
  const px = threshold || 120;
  return el.scrollHeight - el.scrollTop - el.clientHeight < px;
}

function renderConvMessageList(id, total, keepScroll) {
  const el = document.getElementById('conv-messages');
  if (!el) return;
  var remain = Math.max(0, total - S.convMessages.length);
  var loading = remain > 0 ? '<div id="conv-loading-indicator" class="conv-loading"><div class="conv-loading-spinner"></div><span>向上划加载更早消息 (' + remain + ')</span></div>' : '';
  el.innerHTML = loading + '<div class="chat-stream">' + buildRelayActivityHtml(S.convMessages) + '</div>';
  var body = document.querySelector('#page-conv-detail .chat-body');
  if (body) {
    if (_convScrollHandler) body.removeEventListener('scroll', _convScrollHandler);
    _convScrollHandler = function() {
      if (_convLoadingOlder) return;
      if (body.scrollTop > 50) return;
      // Check live state instead of closure
      if (S.convMessages.length >= S.convTotal) return;
      _convLoadingOlder = true;
      var prevHeight = body.scrollHeight;
      loadConvMessages(id, true).then(function() {
        _convLoadingOlder = false;
        requestAnimationFrame(function() {
          var newHeight = body.scrollHeight;
          if (newHeight > prevHeight) body.scrollTop = newHeight - prevHeight;
        });
      }).catch(function() { _convLoadingOlder = false; });
    };
    body.addEventListener('scroll', _convScrollHandler, { passive: true });
    if (!keepScroll) setTimeout(function() { body.scrollTop = body.scrollHeight; }, 0);
  }
}

/**
 * 使用 activity_segment.js 将消息分组并渲染。
 * 每个 tool run 独立显示 banner（自己的 duration/toolCount/phase）。
 */
function buildRelayActivityHtml(messages) {
  if (!messages || !messages.length) return '';
  var segments = buildSegments(messages);
  var groups = buildTurnGroups(segments, false);
  var html = '';
  var runIdx = 0;
  var cardIdx = 0;

  for (var g = 0; g < groups.length; g++) {
    var group = groups[g];
    if (group.userSeg) html += buildConvMessageHtml(group.userSeg.message);

    var runs = buildToolRuns(group);
    var segCursor = 0;

    for (var r = 0; r < runs.length; r++) {
      var run = runs[r];

      // 渲染 run 之前的 chat segments
      for (; segCursor < group.segments.length; segCursor++) {
        var seg = group.segments[segCursor];
        if (seg.type === 'assistant') {
          html += buildConvMessageHtml(seg.message);
        } else if (seg.type === 'user') {
          continue;
        } else {
          break;
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
        html += renderSegmentCard(run.segments[s], 'r' + cardIdx);
        cardIdx++;
      }

      html += '</div></div>';
      runIdx++;

      segCursor += run.segments.length;

      // 渲染 run 之后的 chat segments
      for (; segCursor < group.segments.length; segCursor++) {
        var after = group.segments[segCursor];
        if (after.type === 'assistant') {
          html += buildConvMessageHtml(after.message);
        } else if (after.type === 'user') {
          continue;
        } else {
          break;
        }
      }
    }

    // 渲染末尾剩余 chat segments
    for (; segCursor < group.segments.length; segCursor++) {
      var tail = group.segments[segCursor];
      if (tail.type === 'assistant') {
        html += buildConvMessageHtml(tail.message);
      }
    }
  }
  return html;
}

function buildConvMessageHtml(m) {
  const role = m.role || 'unknown';
  const text = m.text || m.content || m.tool_input_preview || '';
  if (role === 'tool_summary') {
    return '<div class="tool-card">' +
      '<div class="tool-name">' + escHtml(m.tool_name || 'Tool') + '</div>' +
      '<pre>' + escHtml(trunc(text || m.tool_input_full || '', 1600)) + '</pre>' +
      '</div>';
  }
  const mine = role === 'user';
  const cls = mine ? 'user' : 'assistant';
  const label = mine ? '你' : 'Agent';
  return '<div class="chat-msg ' + cls + '">' +
    '<div class="chat-bubble">' +
      '<div class="chat-role">' + label + '</div>' +
      '<div class="chat-text md-render">' + renderMd(text) + '</div>' +
    '</div></div>';
}

async function submitConvPrompt() {
  const conv = S.convDetail;
  const ta = document.getElementById('conv-prompt-input');
  const btn = document.getElementById('conv-send-btn');
  const prompt = ta ? ta.value.trim() : '';
  if (!conv || !prompt) return;
  if (conv.can_resume === false) {
    toast('这个会话只能查看，请新建会话');
    return;
  }

  if (btn) { btn.disabled = true; btn.textContent = '继续中'; }
  if (ta) ta.value = '';
  appendLocalChatMessage({ role: 'user', text: prompt });
  const assistantId = appendLocalChatMessage({ role: 'assistant', text: '已提交，等待 Mac 返回输出...', pending: true });

  try {
    const body = buildContinueJobBody(conv.agent, conv.project_path, conv.conversation_id, prompt);
    await submitContinueJobBody(body, assistantId);
  } catch (e) {
    if (e.code === 'runtime_drift') {
      const ok = confirm('运行环境已漂移，可能会接错模型/权限/工具链。\n\n' + driftText(e.runtime_health) + '\n\n仍然继续？');
      if (ok) {
        const body = buildContinueJobBody(conv.agent, conv.project_path, conv.conversation_id, prompt);
        body.force_confirm = true;
        try {
          await submitContinueJobBody(body, assistantId);
        } catch (again) {
          updateLocalAssistant(assistantId, '提交失败：' + again.message, true);
        }
      } else {
        updateLocalAssistant(assistantId, '已取消：运行环境已漂移', true);
      }
    } else if (e.code === 'resume_cost') {
      const ok = confirm(resumeCostText(e.cost_stats) + '\n\n确认强制继续？');
      if (ok) {
        const body = buildContinueJobBody(conv.agent, conv.project_path, conv.conversation_id, prompt);
        body.force_confirm = true;
        try {
          await submitContinueJobBody(body, assistantId);
        } catch (again) {
          updateLocalAssistant(assistantId, '提交失败：' + again.message, true);
        }
      } else {
        updateLocalAssistant(assistantId, '已取消：继续成本过高', true);
      }
    } else {
      updateLocalAssistant(assistantId, '提交失败：' + e.message, true);
    }
    console.debug('[conv-submit] error:', e.code, e.message);
  } finally {
    if (btn) { btn.disabled = false; btn.textContent = '继续'; }
  }
}

function resumeCostText(stats) {
  stats = stats || {};
  const tokens = Math.round((stats.token_count || 0) / 1000);
  const sizeMB = Math.round((stats.file_size_bytes || 0) / (1024 * 1024));
  return '此会话已消耗较多资源:\n' +
    '  估算 token: ~' + tokens + 'k\n' +
    '  对话记录: ~' + sizeMB + 'MB\n\n继续会增加成本。';
}

async function submitContinueJobBody(body, assistantId) {
  const res = await api('/api/jobs', { method: 'POST', body: JSON.stringify(body) });
  const data = await res.json();
  S.convActiveJob = { id: data.job_id || data.id, assistantId: assistantId, status: 'queued' };
  S.convLogCursor = null;
  updateLocalAssistant(assistantId, '排队中...');
  pollConvJobLogs();
}

// driftText — 来自 shared_ui/runtime_health.js

async function checkRuntimeHealth(id) {
  try {
    const res = await api('/api/conversations/' + id + '/runtime-check');
    const data = await res.json();
    if (S.convDetail && S.convDetail.id === id) {
      S.convDetail.runtime_health = data.health || data.runtime_health;
      renderConvDetailShell(S.convDetail);
      renderConvMessageList(id, S.convMessages.length, true);
    }
    alert(driftText(data.health || data.runtime_health));
  } catch (e) {
    toast('检查失败：' + e.message);
  }
}

function appendLocalChatMessage(m) {
  const id = 'local-' + Date.now() + '-' + Math.random().toString(16).slice(2);
  S.convMessages.push({ ...m, _local_id: id });
  const el = document.getElementById('conv-messages');
  if (el) {
    const total = Math.max(S.convMessages.length, (S.convMessages || []).length);
    renderConvMessageList(S.convDetail.id, total, false);
  }
  return id;
}

function updateLocalAssistant(id, text, failed) {
  const body = document.querySelector('#page-conv-detail .chat-body');
  const shouldFollow = isScrollNearBottom(body);
  S.convMessages = S.convMessages.map(m => m._local_id === id ? { ...m, text: text, failed: !!failed, pending: false } : m);
  if (S.convDetail) renderConvMessageList(S.convDetail.id, S.convMessages.length, !shouldFollow);
}

async function pollConvJobLogs() {
  const job = S.convActiveJob;
  if (!job || !job.id) return;
  try {
    const body = S.convLogCursor ? JSON.stringify({ cursor: S.convLogCursor }) : '{}';
    const res = await api('/api/jobs/' + job.id + '/logs/delta', { method: 'POST', body: body });
    const data = await res.json();
    if (data.logs && data.logs.length) {
      const chunk = data.logs.map(l => cleanAgentLogChunk(l)).filter(Boolean).join('\n');
      if (chunk) {
        const current = (S.convMessages.find(m => m._local_id === job.assistantId) || {}).text || '';
        updateLocalAssistant(job.assistantId, (current === '排队中...' ? '' : current + '\n') + chunk);
      }
      S.convLogCursor = data.cursor || data.next_cursor || S.convLogCursor;
    }
    if (data.status && !['queued', 'running'].includes(data.status)) {
      S.convActiveJob = null;
      pollConvDelta();
      return;
    }
  } catch (e) {
    console.debug('[conv-logs] error:', e.code);
  }
  S.convLogPollTimer = setTimeout(pollConvJobLogs, 2000);
}

function openNewConversationFromDetail() {
  const conv = S.convDetail;
  if (conv) {
    S.pendingRunPrefill = { provider: conv.agent, projectPath: conv.project_path };
  }
  closeConvDetail();
  switchTab('run');
}

function closeConvDetail() {
  clearInterval(S.convDetailPollTimer);
  clearTimeout(S.convLogPollTimer);
  clearTimeout(S.convDeltaTimer);
  S.convDetailPollTimer = null;
  S.convLogPollTimer = null;
  S.convDeltaTimer = null;
  S.convDetail = null;
  S.convMessageSeqs = new Set();
  S.convCursor = 0;
  S.convActiveJob = null;
  document.body.classList.remove('conv-detail-open');
  document.getElementById('page-conv-detail').classList.add('hidden');
  const targetTab = S.prevTabBeforeDetail || 'convos';
  S.tab = targetTab;
  if (targetTab === 'home') {
    document.getElementById('page-home').classList.remove('hidden');
    loadHome();
  } else {
    document.getElementById('page-convos').classList.remove('hidden');
    loadConversations();
  }
}

// ============================================================
// Run tab
// ============================================================
async function loadRunContext() {
  const el = document.getElementById('page-run');
  if (S.runCtx) { renderRun(); return; }
  el.innerHTML = '<h2>执行</h2>' + skelCard() + skelCard();
  try {
    const res = await api('/api/run/context');
    S.runCtx = await res.json();
    console.debug('[run] context loaded: %d projects, providers: %j',
      (S.runCtx.projects || []).length, S.runCtx.provider_availability);
    renderRun();
  } catch (e) {
    console.debug('[run] context error:', e.code, e.message);
    el.innerHTML = '<h2>执行</h2>' +
      '<div class="error-state"><div class="empty-icon">&#9888;</div>' +
      '<div>' + escHtml(e.message) + '</div>' +
      '<button class="retry-btn" onclick="S.runCtx=null;loadRunContext()">重试</button></div>';
  }
}

function renderRun() {
  const el = document.getElementById('page-run');
  const ctx = S.runCtx || {};
  const projects = ctx.projects || [];
  const providers = ctx.provider_availability || [];
  const providerList = Array.isArray(providers)
    ? providers
    : Object.entries(providers).map(([provider, value]) => ({
        provider,
        available: value && value.available !== false,
      }));

  const providerOpts = providerList.map(p => {
    const key = p.provider || p.key || '';
    const label = agentLabel(key);
    const avail = p.available !== false;
    return '<option value="' + escHtml(key) + '"' + (avail ? '' : ' disabled') + '>' + escHtml(label + (avail ? '' : ' (不可用)')) + '</option>';
  }).join('');

  const projectOpts = projects.map(p => {
    const name = typeof p === 'string' ? p : (p.name || p.path || p);
    const path = typeof p === 'string' ? p : (p.path || p);
    return '<option value="' + escHtml(path) + '">' + escHtml(shortProject(path) || name) + '</option>';
  }).join('');

  el.innerHTML = '<h2>执行</h2>' +
    '<div class="card">' +
    '<div class="card-title">Provider</div>' +
    '<select id="run-provider">' + (providerOpts || '<option value="claude_code">Claude Code</option>') + '</select>' +
    (projectOpts ? '<label>项目</label><select id="run-project">' + projectOpts + '</select>' : '') +
    '<label>Prompt</label>' +
    '<textarea id="run-prompt" placeholder="输入指令..."></textarea>' +
    '<div style="margin-top:10px"><button onclick="submitRun()" id="run-submit-btn">执行</button></div>' +
    '</div>' +
    '<div id="run-output-card" class="hidden">' +
    '<div class="card">' +
    '<div class="job-status-bar">' +
    '<div class="card-title" style="margin:0">输出</div>' +
    '<div style="display:flex;align-items:center;gap:8px">' +
    '<span id="run-status-badge"></span>' +
    '<button class="sm danger hidden" id="run-cancel-btn" onclick="cancelJob()">取消</button>' +
    '</div></div>' +
    '<div id="run-output" class="run-output"></div>' +
    '</div></div>' +
    '<div id="run-history"></div>';

  loadRunHistory();

  // Apply pending pre-fill from openNewConversationFromDetail()
  if (S.pendingRunPrefill) {
    const pf = S.pendingRunPrefill;
    S.pendingRunPrefill = null;
    const provider = document.getElementById('run-provider');
    if (provider && pf.provider) provider.value = pf.provider;
    const project = document.getElementById('run-project');
    if (project && pf.projectPath) project.value = pf.projectPath;
    const prompt = document.getElementById('run-prompt');
    if (prompt) prompt.focus();
  }
}

async function submitRun() {
  const provider = (document.getElementById('run-provider') || {}).value || 'claude_code';
  const project = (document.getElementById('run-project') || {}).value || '';
  const prompt = (document.getElementById('run-prompt') || {}).value.trim();
  if (!prompt) return;

  const btn = document.getElementById('run-submit-btn');
  btn.disabled = true;
  btn.textContent = '提交中...';

  try {
    const body = buildNewJobBody(provider, project, prompt);
    const res = await api('/api/jobs', { method: 'POST', body: JSON.stringify(body) });
    const data = await res.json();
    S.activeJob = { id: data.id || data.job_id, status: 'queued' };
    S.logCursor = null;
    console.debug('[run] job submitted:', S.activeJob.id);

    document.getElementById('run-output-card').classList.remove('hidden');
    document.getElementById('run-output').textContent = '';
    document.getElementById('run-status-badge').innerHTML = '<span class="badge badge-yellow">排队中</span>';
    document.getElementById('run-cancel-btn').classList.remove('hidden');
    btn.textContent = '执行中...';
    pollJobStatus();
    pollJobLogs();
  } catch (e) {
    console.debug('[run] submit error:', e.code, e.message);
    alert('提交失败: ' + e.message);
  } finally {
    btn.disabled = false;
    btn.textContent = '执行';
  }
}

async function pollJobStatus() {
  if (!S.activeJob) return;
  try {
    const res = await api('/api/jobs/' + S.activeJob.id);
    const data = await res.json();
    S.activeJob.status = data.status;
    const badge = document.getElementById('run-status-badge');
    if (badge) badge.innerHTML = jobBadge(data.status);
    console.debug('[run] job status:', data.status);
    if (data.status === 'succeeded' || data.status === 'failed' || data.status === 'cancelled' || data.status === 'timeout') {
      const cancelBtn = document.getElementById('run-cancel-btn');
      if (cancelBtn) cancelBtn.classList.add('hidden');
      return; // terminal
    }
  } catch (e) {
    console.debug('[run] poll status error:', e.code);
  }
  setTimeout(pollJobStatus, 3000);
}

async function pollJobLogs() {
  if (!S.activeJob) return;
  try {
    const body = S.logCursor ? JSON.stringify({ cursor: S.logCursor }) : '{}';
    const res = await api('/api/jobs/' + S.activeJob.id + '/logs/delta', {
      method: 'POST', body: body
    });
    const data = await res.json();
    if (data.logs && data.logs.length > 0) {
      const container = document.getElementById('run-output');
      if (container) {
        data.logs.forEach(l => {
          const span = document.createElement('div');
          span.className = 'log-line';
          span.textContent = l.chunk || l.text || '';
          container.appendChild(span);
        });
        container.scrollTop = container.scrollHeight;
      }
      S.logCursor = data.cursor || data.next_cursor;
    }
    if (data.status && data.status !== 'running' && data.status !== 'queued') {
      const badge = document.getElementById('run-status-badge');
      if (badge) badge.innerHTML = jobBadge(data.status);
      return;
    }
  } catch (e) {
    console.debug('[run] poll logs error:', e.code);
  }
  S.logPollTimer = setTimeout(pollJobLogs, 2000);
}

async function cancelJob() {
  if (!S.activeJob) return;
  console.debug('[run] cancel job:', S.activeJob.id);
  try {
    await api('/api/jobs/' + S.activeJob.id + '/cancel', { method: 'POST' });
    console.debug('[run] cancel submitted');
    const badge = document.getElementById('run-status-badge');
    if (badge) badge.innerHTML = jobBadge('cancelled');
    const cancelBtn = document.getElementById('run-cancel-btn');
    if (cancelBtn) cancelBtn.classList.add('hidden');
  } catch (e) {
    console.debug('[run] cancel error:', e.code, e.message);
    alert('取消失败: ' + e.message);
  }
}

async function loadRunHistory() {
  const el = document.getElementById('run-history');
  if (!el) return;
  try {
    const res = await api('/api/jobs?limit=5');
    const data = await res.json();
    const jobs = data.jobs || [];
    if (jobs.length === 0) return;
    el.innerHTML = '<div class="card" style="margin-top:12px">' +
      '<div class="card-title">历史任务</div>' +
      jobs.map(j => {
        const prompt = j.prompt || j.kind || '';
        return '<div class="conv-item">' +
          '<div class="conv-header">' +
          '<span class="conv-title">' + escHtml(trunc(prompt, 50)) + '</span>' +
          jobBadge(j.status) +
          '</div></div>';
      }).join('') + '</div>';
  } catch (e) {
    console.debug('[run] history error:', e.code);
  }
}

// ============================================================
// Settings tab
// ============================================================
function renderSettings() {
  const el = document.getElementById('page-settings');
  const token = S.token;
  const masked = token.length > 16 ? token.substring(0, 8) + '...' + token.substring(token.length - 8) : token;
  const relayUrl = window.location.origin;
  const beatRtt = S.beat.rtt_ms !== null ? S.beat.rtt_ms + 'ms' : '--';
  const macStatus = S.beat.status === 'online' || S.health.status === 'online'
    ? '<span style="color:#22c55e">在线</span>'
    : '<span style="color:#ef4444">离线</span>';

  el.innerHTML = '<h2>设置</h2>' +
    '<div class="card">' +
    '<div class="setting-row"><span class="setting-label">Relay 域名</span><span class="setting-value">' + escHtml(relayUrl) + '</span></div>' +
    '<div class="setting-row"><span class="setting-label">Token</span><span class="setting-value" style="font-family:monospace;font-size:12px">' + escHtml(masked) + '</span></div>' +
    '<div class="setting-row"><span class="setting-label">Mac 状态</span><span class="setting-value">' + macStatus + '</span></div>' +
    '<div class="setting-row"><span class="setting-label">延迟</span><span class="setting-value">' + beatRtt + '</span></div>' +
    '<div class="setting-row"><span class="setting-label">版本</span><span class="setting-value">v__VERSION__</span></div>' +
    '<div class="setting-row"><span class="setting-label">构建时间</span><span class="setting-value">__BUILD_TIME__</span></div>' +
    '</div>' +
    '<div style="margin-top:12px"><button class="danger" onclick="logout()">退出登录</button></div>';
}

// escHtml, jsStr, trunc, agentLabel, shortProject, shortId, relTime — 来自 shared_ui/view_model.js

function jobBadge(status) {
  const map = {
    'queued': ['badge-yellow', '排队中'],
    'running': ['badge-blue', '运行中'],
    'succeeded': ['badge-green', '成功'],
    'failed': ['badge-red', '失败'],
    'cancelled': ['badge-gray', '已取消'],
    'timeout': ['badge-red', '空闲超时'],
    'observing': ['badge-orange', '等待返回'],
  };
  const [cls, label] = map[status] || ['badge-gray', status || '未知'];
  return '<span class="badge ' + cls + '">' + label + '</span>';
}

function emptyReason(data) {
  if (data && data._error) {
    const code = data._error.code;
    if (code === 'auth_failed') return 'Token 无效，请重新登录';
    if (code === 'mac_offline') return 'Mac 不在线，无法获取会话';
    if (code === 'network_error') return '网络连接失败';
    return '请求失败: ' + (data._error.message || '未知错误');
  }
  if (data && data.conversations && data.conversations.length === 0) {
    return '暂无会话';
  }
  if (data && data.conversations === null) {
    return 'Mac 返回为空';
  }
  return '暂无会话';
}

function skelCard() {
  return '<div class="card"><div class="skeleton skel-line short"></div><div class="skeleton skel-line"></div><div class="skeleton skel-line medium"></div></div>';
}

function skelLines(n) {
  let s = '';
  for (let i = 0; i < n; i++) s += '<div class="skeleton skel-line' + (i % 3 === 0 ? ' short' : i % 3 === 1 ? '' : ' medium') + '"></div>';
  return s;
}

// ============================================================
// Boot
// ============================================================
if (typeof module !== 'undefined' && module.exports) {
  module.exports = {};
}

if (typeof window !== 'undefined') {
  loadSavedToken();
}
