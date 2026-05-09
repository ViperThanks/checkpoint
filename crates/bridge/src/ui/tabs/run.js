/* run.js — Run tab */

const RS = {
  kind: 'agent_prompt',
  activeJobId: null,
  pollTimer: null,
  ctx: null,
  sessionMode: 'new',
  selectedProject: '',
  selectedProvider: '',
  chatMessages: [],
  activeAssistantMessageId: null,
  activeUserMessageId: null,
  lastSubmitBody: null,
  logAfterId: 0,
  logLineCount: 0,
  assistantTextBuffer: ''
};

window.RS = RS;

const TEMPLATES = [
  { id: 'status', label: '状态检查', prompt: '运行 agent-aspect status 并报告结果' },
  { id: 'git', label: 'Git 状态', prompt: '运行 git status 并报告结果' },
  { id: 'test', label: 'Cargo 测试', prompt: '运行 cargo test 并报告测试结果' },
  { id: 'smoke', label: '冒烟测试', prompt: '运行 scripts/smoke_test.sh 并报告结果' },
  { id: 'mode', label: '模式设置', prompt: '运行 agent-aspect mode guard' }
];

/* ---------- Layout ---------- */
function ensureRunLayout() {
  const view = document.getElementById('run-view');
  if (!view || document.getElementById('run-form-scroll')) return;

  view.innerHTML =
    '<div class="run-layout">' +
      '<div class="run-form-panel">' +
        '<div class="run-form-scroll" id="run-form-scroll">' +
          '<div class="section-label">1. 选择 Provider</div>' +
          '<div class="provider-grid" id="provider-grid"></div>' +

          '<div class="section-label">2. 选择项目</div>' +
          '<div class="project-grid" id="project-list"></div>' +

          '<div class="section-label">3. 会话模式</div>' +
          '<div class="session-toggle">' +
            '<div class="session-toggle-btn active" id="sess-new" onclick="setSessionMode(\'new\')">新建</div>' +
            '<div class="session-toggle-btn" id="sess-continue" onclick="setSessionMode(\'continue\')">继续</div>' +
          '</div>' +

          '<div id="session-conv-select" class="hidden">' +
            '<div class="section-label">4. 选择会话</div>' +
            '<select class="select" id="ctx-conversation"><option value="">选择会话...</option></select>' +
          '</div>' +

          '<div class="section-label">5. 提示词</div>' +
          '<div class="template-bar" id="template-bar"></div>' +
          '<textarea class="textarea" id="job-prompt-input" placeholder="输入提示词，或使用上方模板..." style="min-height:100px"></textarea>' +

          '<button class="btn btn-primary" id="run-submit" onclick="submitJob()" style="width:100%;margin-top:12px">提交任务</button>' +
        '</div>' +
      '</div>' +
      '<div class="run-log-panel">' +
        '<div class="run-log-header">对话</div>' +
        '<div id="run-log-area" style="flex:1;overflow-y:auto;padding:12px">' +
          '<div id="run-chat-thread" class="run-chat-thread"></div>' +
          '<div id="job-active-area" class="hidden" style="display:flex;flex-direction:column;gap:10px;margin-bottom:16px">' +
            '<div class="job-status-bar">' +
              '<span id="job-status-badge"></span>' +
              '<span id="job-completed-reason" class="job-completed-reason"></span>' +
              '<span id="job-kind-label"></span>' +
              '<button class="job-cancel hidden" id="job-cancel-btn" onclick="cancelActiveJob()">取消</button>' +
            '</div>' +
            '<details class="job-raw-details">' +
              '<summary>原始日志</summary>' +
              '<div class="job-log" id="job-log-output"></div>' +
            '</details>' +
          '</div>' +
          '<div id="job-history-list"></div>' +
        '</div>' +
      '</div>' +
    '</div>';
}

function renderTemplates() {
  const bar = document.getElementById('template-bar');
  if (!bar) return;
  let h = '';
  TEMPLATES.forEach(function (t) {
    h += '<button class="template-chip" onclick="applyTemplate(\'' + jsStr(t.id) + '\')">' + esc(t.label) + '</button>';
  });
  bar.innerHTML = h;
}

function applyTemplate(id) {
  const t = TEMPLATES.find(function (x) { return x.id === id; });
  if (!t) return;
  const ta = document.getElementById('job-prompt-input');
  if (ta) ta.value = t.prompt;
}

/* ---------- Context ---------- */
function loadRunContext() {
  ensureRunLayout();
  renderTemplates();
  api('/run/context').then(function (r) {
    if (r.error) return;
    RS.ctx = r;

    // Provider grid
    const pg = document.getElementById('provider-grid');
    if (pg) {
      let ph = '';
      ['claude_code', 'kimi_code', 'codex_cli'].forEach(function (p) {
        const avail = (r.provider_availability || []).find(function (x) { return x.provider === p; });
        const isAvail = avail && avail.available;
        const label = AGENTS[p] || p;
        const cls = 'provider-btn' + (RS.selectedProvider === p ? ' active' : '') + (isAvail ? '' : ' unavailable');
        ph += '<div class="' + cls + '" data-provider="' + esc(p) + '" onclick="selectProvider(\'' + jsStr(p) + '\')">' + esc(label) + '</div>';
      });
      pg.innerHTML = ph;
    }

    // Project list
    const pl = document.getElementById('project-list');
    if (pl) {
      let phtml = '';
      (r.projects || []).forEach(function (p) {
        phtml += '<div class="project-card' + (RS.selectedProject === p.path ? ' active' : '') + '" data-path="' + esc(p.path) + '" onclick="selectProject(\'' + jsStr(p.path) + '\')">' +
          '<span>' + esc(projectBasename(p.path)) + '</span>' +
          '<span style="font-size:.66rem;color:var(--dim);margin-left:auto">' + (p.agents || []).map(function (a) { return AGENTS[a] || a; }).join(', ') + '</span></div>';
      });
      if (!phtml) phtml = '<div style="color:var(--dimmer);font-size:.78rem;padding:4px 0">暂无项目</div>';
      pl.innerHTML = phtml;
    }

    updateConvSelect();
  });
}

function selectProvider(p) {
  RS.selectedProvider = p;
  document.querySelectorAll('.provider-btn').forEach(function (b) {
    b.classList.toggle('active', b.dataset.provider === p);
  });
  updateConvSelect();
}

function selectProject(p) {
  RS.selectedProject = p;
  document.querySelectorAll('.project-card').forEach(function (c) {
    c.classList.toggle('active', c.dataset.path === p);
  });
  updateConvSelect();
}

function setSessionMode(mode) {
  RS.sessionMode = mode;
  const n = document.getElementById('sess-new');
  const c = document.getElementById('sess-continue');
  if (n) n.classList.toggle('active', mode === 'new');
  if (c) c.classList.toggle('active', mode === 'continue');
  const cs = document.getElementById('session-conv-select');
  if (cs) cs.classList.toggle('hidden', mode !== 'continue');
}

function updateConvSelect() {
  const sel = document.getElementById('ctx-conversation');
  if (!sel) return;
  if (!RS.ctx || !RS.selectedProvider) {
    sel.innerHTML = '<option value="">选择会话...</option>';
    return;
  }
  let convs = (RS.ctx.recent_conversations || []).filter(function (c) {
    return c.agent === RS.selectedProvider && c.can_resume !== false;
  });
  if (RS.selectedProject) convs = convs.filter(function (c) { return c.project_path === RS.selectedProject; });
  if (!convs.length) {
    const note = RS.selectedProvider === 'codex_cli'
      ? 'Codex App/GUI 会话仅可查看，请新建会话'
      : '无可继续会话';
    sel.innerHTML = '<option value="">' + esc(note) + '</option>';
    return;
  }
  sel.innerHTML = '<option value="">选择会话...</option>' + convs.map(function (c) {
    return '<option value="' + esc(c.conversation_id) + '">' + esc(c.title || shortId(c.conversation_id)) + '</option>';
  }).join('');
}

/* ---------- Submit ---------- */
function submitJob() {
  const pi = document.getElementById('job-prompt-input');
  const prompt = pi ? pi.value.trim() : '';
  if (!prompt) { toast('请输入提示词'); return; }
  if (!RS.selectedProvider) { toast('请选择代理'); return; }
  const avail = (RS.ctx && RS.ctx.provider_availability || []).find(function (x) { return x.provider === RS.selectedProvider; });
  if (!avail || !avail.available) {
    toast('代理不可用: ' + (avail && avail.error ? avail.error : '未找到'));
    return;
  }

  var body;
  const cv = document.getElementById('ctx-conversation');
  if (RS.sessionMode === 'continue') {
    if (!cv || !cv.value) {
      toast('请选择要继续的会话');
      return;
    }
    body = buildContinueJobBody(RS.selectedProvider, RS.selectedProject, cv.value, prompt);
  } else {
    body = buildNewJobBody(RS.selectedProvider, RS.selectedProject, prompt);
  }
  RS.lastSubmitBody = JSON.parse(JSON.stringify(body));
  const userId = 'u-' + Date.now();
  const assistantId = 'a-' + Date.now();
  RS.activeUserMessageId = userId;
  RS.activeAssistantMessageId = assistantId;
  RS.chatMessages = [
    {
      id: userId,
      role: 'user',
      text: prompt,
      status: 'sending',
      provider: RS.selectedProvider,
      project: RS.selectedProject
    },
    {
      id: assistantId,
      role: 'assistant',
      text: providerLabel(RS.selectedProvider) + ' 正在处理...',
      status: 'queued',
      provider: RS.selectedProvider
    }
  ];
  renderRunChat();

  const btn = document.getElementById('run-submit');
  if (btn) btn.disabled = true;
  api('/jobs', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body)
  }).then(function (r) {
    if (btn) btn.disabled = false;
    if (r.error) {
      if (r.runtime_health && r.runtime_health.status === 'critical') {
        confirmRunForceContinue(
          '运行环境发生漂移，强制继续可能导致接错模型/权限/工具链。\n\n' +
            driftText(r.runtime_health),
          body,
          btn
        );
        return;
      }
      if (r.cost_stats) {
        confirmRunForceContinue(resumeCostText(r.cost_stats), body, btn);
        return;
      }
      var errMsg = r.error;
      if (errMsg.indexOf('not CLI-resumable') >= 0 || errMsg.indexOf('codex_not_resumable') >= 0) {
        errMsg = 'Codex 会话不支持 CLI 继续，请新建会话';
      }
      markRunChatFailed('提交失败: ' + errMsg);
      toast('错误: ' + errMsg);
      return;
    }
    RS.activeJobId = r.job_id;
    RS.logAfterId = 0;
    RS.logLineCount = 0;
    RS.assistantTextBuffer = '';
    bindActiveChatJob(r.job_id);
    showJobActive(r.job_id, 'agent_prompt', 'queued');
    pollJobStatus();
    loadRunContext();
    if (S.tab === 'conv' && typeof loadConvList === 'function') loadConvList();
    if (pi) pi.value = '';
  }).catch(function () {
    if (btn) btn.disabled = false;
    markRunChatFailed('提交失败');
    toast('提交失败');
  });
}

function confirmRunForceContinue(message, body, btn) {
  if (!confirm(message + '\n\n确认强制继续？')) {
    markRunChatFailed('已取消');
    toast('已取消');
    return;
  }
  const forced = JSON.parse(JSON.stringify(body));
  forced.force_confirm = true;
  if (btn) btn.disabled = true;
  api('/jobs', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(forced)
  }).then(function (r) {
    if (btn) btn.disabled = false;
    if (r.error) {
      markRunChatFailed('提交失败: ' + r.error);
      toast('错误: ' + r.error);
      return;
    }
    RS.activeJobId = r.job_id;
    RS.logAfterId = 0;
    RS.logLineCount = 0;
    RS.assistantTextBuffer = '';
    bindActiveChatJob(r.job_id);
    showJobActive(r.job_id, 'agent_prompt', 'queued');
    pollJobStatus();
    loadRunContext();
  }).catch(function () {
    if (btn) btn.disabled = false;
    markRunChatFailed('提交失败');
    toast('提交失败');
  });
}

function resumeCostText(stats) {
  stats = stats || {};
  const tokens = Math.round((stats.token_count || 0) / 1000);
  const sizeMB = Math.round((stats.file_size_bytes || 0) / (1024 * 1024));
  return '此会话已消耗较多资源:\n' +
    '  估算 token: ~' + tokens + 'k\n' +
    '  对话记录: ~' + sizeMB + 'MB\n\n继续会增加成本。';
}

function providerLabel(provider) {
  return AGENTS[provider] || provider || 'Agent';
}

function humanFailureReason(reason) {
  if (!reason) return '任务失败';
  if (reason.indexOf('spawn failed') === 0) return 'Provider 二进制未找到，请检查配置';
  if (reason.indexOf('idle timeout') === 0) {
    return reason.replace(/idle timeout after (\d+)s without output/, '空闲超时（$1秒内无输出）');
  }
  if (reason.indexOf('timeout') === 0) return reason.replace(/timeout after (\d+)s/, '空闲超时（$1秒）');
  if (reason === 'process exited with non-zero status') return '进程异常退出';
  if (reason === 'process ended without exit status') return '进程意外终止';
  if (reason === 'cancelled by user') return '任务已取消';
  return reason;
}

function bindActiveChatJob(jobId) {
  RS.chatMessages.forEach(function (m) {
    if (m.id === RS.activeAssistantMessageId || m.id === RS.activeUserMessageId) {
      m.jobId = jobId;
    }
  });
  renderRunChat();
}

function renderRunChat() {
  const el = document.getElementById('run-chat-thread');
  if (!el) return;
  if (!RS.chatMessages.length) {
    el.innerHTML = '<div class="run-chat-empty">选择代理和项目，发送一条提示词开始。</div>';
    return;
  }
  el.innerHTML = RS.chatMessages.map(function (m) {
    const cls = 'run-msg run-msg-' + esc(m.role) + ' run-msg-' + esc(m.status || 'idle');
    let meta = '';
    if (m.role === 'assistant') {
      meta = '<div class="run-msg-meta">' + esc(providerLabel(m.provider)) + statusInline(m.status) + '</div>';
    } else {
      const parts = [];
      if (m.provider) parts.push(providerLabel(m.provider));
      if (m.project) parts.push(projectBasename(m.project));
      meta = '<div class="run-msg-meta">' + esc(parts.join(' · ') || '你') + statusInline(m.status) + '</div>';
    }
    let body = '<div class="run-msg-text">' + renderMsgText(m.text) + '</div>';
    if (m.status === 'failed') {
      body += '<button class="run-retry-btn" onclick="retryLastPrompt()">重试</button>';
    }
    return '<div class="' + cls + '">' + meta + body + '</div>';
  }).join('');
  el.scrollTop = el.scrollHeight;
}

function renderMsgText(text) {
  const safe = esc(text || '');
  return safe.replace(/\n/g, '<br>');
}

function statusInline(status) {
  if (status === 'sending' || status === 'queued' || status === 'running') {
    return '<span class="run-spinner"></span>';
  }
  if (status === 'failed') return '<span class="run-error-icon">!</span>';
  if (status === 'succeeded') return '<span class="run-ok-icon">✓</span>';
  return '';
}

function setRunMessage(id, patch) {
  RS.chatMessages = RS.chatMessages.map(function (m) {
    if (m.id !== id) return m;
    return Object.assign({}, m, patch);
  });
  renderRunChat();
}

function markRunChatFailed(reason) {
  if (RS.activeUserMessageId) setRunMessage(RS.activeUserMessageId, { status: 'idle' });
  if (RS.activeAssistantMessageId) {
    setRunMessage(RS.activeAssistantMessageId, {
      status: 'failed',
      text: reason || '任务失败'
    });
  }
}

function retryLastPrompt() {
  if (!RS.lastSubmitBody) {
    toast('没有可重试的任务');
    return;
  }
  const pi = document.getElementById('job-prompt-input');
  if (pi && RS.lastSubmitBody.prompt) pi.value = RS.lastSubmitBody.prompt;
  if (RS.lastSubmitBody.provider) selectProvider(RS.lastSubmitBody.provider);
  if (RS.lastSubmitBody.project_path) selectProject(RS.lastSubmitBody.project_path);
  if (RS.lastSubmitBody.conversation_id) {
    setSessionMode('continue');
    setTimeout(function () {
      const cv = document.getElementById('ctx-conversation');
      if (cv) cv.value = RS.lastSubmitBody.conversation_id;
    }, 0);
  }
  submitJob();
}

/* ---------- Active job ---------- */
function showJobActive(id, kind, status) {
  RS.logAfterId = 0;
  RS.logLineCount = 0;
  RS.assistantTextBuffer = '';
  const area = document.getElementById('job-active-area');
  if (area) area.classList.remove('hidden');
  const kl = document.getElementById('job-kind-label');
  if (kl) kl.textContent = jobKindLabel(kind);
  updateJobStatusBadge(status);
  const cancelBtn = document.getElementById('job-cancel-btn');
  if (cancelBtn) cancelBtn.classList.toggle('hidden', status !== 'running' && status !== 'queued');
  const logOut = document.getElementById('job-log-output');
  if (logOut) logOut.innerHTML = '';
}

function updateJobStatusBadge(status, completedReason, completion) {
  const badgeEl = document.getElementById('job-status-badge');
  if (!badgeEl) return;
  const view = jobStatusView(status);
  badgeEl.textContent = view.label;
  badgeEl.className = 'badge';
  badgeEl.style.background = '';
  if (view.bridgeType) badgeEl.classList.add('badge-' + view.bridgeType);
  else badgeEl.style.background = 'var(--surface3)';
  var crEl = document.getElementById('job-completed-reason');
  if (crEl) {
    var crText = humanCompletedReason(completedReason);
    var detail = humanCompletionDetail(completion);
    if ((crText || detail) && terminalJobStatus(status)) {
      crEl.textContent = [crText, detail].filter(Boolean).join(' · ');
      crEl.classList.remove('hidden');
    } else {
      crEl.textContent = '';
      crEl.classList.add('hidden');
    }
  }
}

function pollJobStatus() {
  if (RS.pollTimer) clearInterval(RS.pollTimer);
  RS.pollTimer = setInterval(function () {
    if (!RS.activeJobId) return;
    api('/jobs/' + RS.activeJobId).then(function (r) {
      if (r.error) return;
      updateJobStatusBadge(r.status, r.completed_reason, r.completion);
      const cancelBtn = document.getElementById('job-cancel-btn');
      if (cancelBtn) cancelBtn.classList.toggle('hidden', r.status !== 'running' && r.status !== 'queued' && r.status !== 'observing');
      if (r.status === 'succeeded' || r.status === 'failed' || r.status === 'cancelled' || r.status === 'timeout') {
        clearInterval(RS.pollTimer);
        RS.pollTimer = null;
        refreshActiveJob();
        loadJobHistory();
      }
    });
  }, 2000);
}

// Lightweight SSE-driven refresh: update badge from payload, only full-refresh on terminal
function refreshActiveJobFromSSE(payload) {
  if (!RS.activeJobId || payload.job_id !== RS.activeJobId) return;
  updateJobStatusBadge(payload.status, payload.completed_reason, {
    signal: payload.completion_signal,
    authority: payload.completion_authority
  });
  var cancelBtn = document.getElementById('job-cancel-btn');
  if (cancelBtn) cancelBtn.classList.toggle('hidden', payload.status !== 'running' && payload.status !== 'queued' && payload.status !== 'observing');
  if (payload.status === 'succeeded' || payload.status === 'failed' || payload.status === 'cancelled' || payload.status === 'timeout') {
    if (RS.pollTimer) { clearInterval(RS.pollTimer); RS.pollTimer = null; }
    // Final refresh for full job details + remaining logs
    refreshActiveJob();
    loadJobHistory();
  }
}

function refreshActiveJob() {
  if (!RS.activeJobId) return;
  api('/jobs/' + RS.activeJobId).then(function (r) {
    if (r.error) return;
    updateJobStatusBadge(r.status, r.completed_reason, r.completion);
    updateChatFromJob(r, null);
    const cancelBtn = document.getElementById('job-cancel-btn');
    if (cancelBtn) cancelBtn.classList.toggle('hidden', r.status !== 'running' && r.status !== 'queued');
  });
  api('/jobs/' + RS.activeJobId + '/logs/delta', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ after_id: RS.logAfterId || 0, limit: 50 })
  }).then(function (r) {
    if (r.error) return;
    const el = document.getElementById('job-log-output');
    if (!el) return;
    let html = '';
    (r.logs || []).forEach(function (l) {
      const cls = l.stream === 'stdout' ? 'log-stdout' : l.stream === 'stderr' ? 'log-stderr' : 'log-system';
      html += '<span class="' + cls + '">' + esc(l.chunk).replace(/\n/g, '<br>') + '</span><br>';
    });
    if (html) {
      if (el.dataset.empty === '1') {
        el.innerHTML = '';
        delete el.dataset.empty;
      }
      el.insertAdjacentHTML('beforeend', html);
      RS.logLineCount += (r.logs || []).length;
      trimRawLogOutput(el, 180);
      el.scrollTop = el.scrollHeight;
    } else if (!el.innerHTML.trim()) {
      el.innerHTML = '<span style="color:var(--dimmer)">暂无输出</span>';
      el.dataset.empty = '1';
    }
    RS.logAfterId = r.next_after_id || RS.logAfterId || 0;
    updateChatFromLogs(r.logs || []);
  });
}

function updateChatFromJob(job, logs) {
  if (!RS.chatMessages.length && job.prompt) {
    RS.chatMessages = [
      {
        id: 'u-' + job.id,
        role: 'user',
        text: job.prompt,
        status: 'idle',
        provider: job.provider,
        project: job.project_path,
        jobId: job.id
      },
      {
        id: 'a-' + job.id,
        role: 'assistant',
        text: providerLabel(job.provider) + ' 正在处理...',
        status: job.status,
        provider: job.provider,
        jobId: job.id
      }
    ];
    RS.activeUserMessageId = 'u-' + job.id;
    RS.activeAssistantMessageId = 'a-' + job.id;
  }
  if (RS.activeUserMessageId) setRunMessage(RS.activeUserMessageId, { status: 'idle' });
  if (!RS.activeAssistantMessageId) return;
  let text = providerLabel(job.provider) + ' 正在处理...';
  if (job.status === 'failed') {
    text = humanFailureReason(job.failure_reason || job.completed_reason);
  } else if (job.status === 'cancelled') {
    text = humanFailureReason(job.failure_reason || job.completed_reason);
  } else if (job.status === 'succeeded') {
    var cr = humanCompletedReason(job.completed_reason);
    text = '任务已完成。' + (cr ? '（' + cr + '）' : '');
  } else if (job.status === 'timeout') {
    text = humanFailureReason(job.completed_reason || job.failure_reason) || '任务超时';
  }
  setRunMessage(RS.activeAssistantMessageId, { status: job.status, text: text, provider: job.provider });
  if (logs) updateChatFromLogs(logs);
}

function updateChatFromLogs(logs) {
  if (!RS.activeAssistantMessageId) return;
  const cleaned = (logs || []).map(cleanAgentLogChunk).filter(Boolean).join('\n').trim();
  if (!cleaned) return;
  const msg = RS.chatMessages.find(function (m) { return m.id === RS.activeAssistantMessageId; });
  if (!msg) return;
  RS.assistantTextBuffer = (RS.assistantTextBuffer ? RS.assistantTextBuffer + '\n' : '') + cleaned;
  if (RS.assistantTextBuffer.length > 12000) {
    RS.assistantTextBuffer = RS.assistantTextBuffer.slice(-12000);
  }
  const status = msg.status === 'queued' ? 'running' : msg.status;
  setRunMessage(RS.activeAssistantMessageId, { status: status, text: RS.assistantTextBuffer.trim() });
}

// cleanAgentLogChunk is now defined in app.js (shared with conversations.js)

function trimRawLogOutput(el, maxLines) {
  while (RS.logLineCount > maxLines && el.childNodes.length > 2) {
    el.removeChild(el.firstChild);
    if (el.firstChild) el.removeChild(el.firstChild);
    RS.logLineCount -= 1;
  }
}

function cancelActiveJob() {
  if (!RS.activeJobId || !confirm('确认取消此任务?')) return;
  api('/jobs/' + RS.activeJobId + '/cancel', { method: 'POST' }).then(function (r) {
    if (r.error) { toast('错误: ' + r.error); return; }
    toast('已取消');
    if (RS.pollTimer) { clearInterval(RS.pollTimer); RS.pollTimer = null; }
    refreshActiveJob();
    loadJobHistory();
  });
}

/* ---------- History ---------- */
function loadJobHistory() {
  ensureRunLayout();
  api('/jobs?limit=10').then(function (r) {
    if (r.error) return;
    const list = document.getElementById('job-history-list');
    if (!list) return;
    const jobs = r.jobs || [];
    if (!jobs.length) {
      list.innerHTML = '<div style="color:var(--dimmer);font-size:.75rem;padding:8px 0">暂无任务</div>';
      return;
    }
    list.innerHTML = jobs.map(function (j) {
      let h = '<div class="job-history-item" onclick="viewJob(\'' + jsStr(j.id) + '\')">' +
        '<div class="job-history-top">' +
        renderBridgeJobBadge(j.status) +
        '<span class="job-history-kind">' + esc(jobKindLabel(j.kind)) + '</span>' +
        '<span class="job-history-time">' + ago(j.created_at) + '</span></div>';
      if (j.project_path || j.provider || j.prompt) {
        h += '<div class="job-ctx-row">';
        if (j.project_path) h += '<span class="job-ctx-tag">' + esc(projectBasename(j.project_path)) + '</span>';
        if (j.provider) h += '<span class="job-ctx-tag">' + esc(AGENTS[j.provider] || j.provider) + '</span>';
        if (j.prompt) h += '<span class="job-ctx-tag">' + esc(j.prompt.slice(0, 40) + (j.prompt.length > 40 ? '...' : '')) + '</span>';
        h += '</div>';
      }
      return h + '</div>';
    }).join('');
  });
}

function viewJob(id) {
  RS.activeJobId = id;
  RS.logAfterId = 0;
  api('/jobs/' + id).then(function (r) {
    if (r.error) return;
    RS.chatMessages = [];
    RS.activeUserMessageId = null;
    RS.activeAssistantMessageId = null;
    showJobActive(id, r.kind, r.status);
    updateChatFromJob(r, null);
    refreshActiveJob();
  });
}
