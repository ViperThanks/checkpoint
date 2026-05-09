// ===== Home Tab =====
// Dashboard: mode, pending, jobs, conversations, providers, relay pairing

function loadHome() {
  const view = document.getElementById('home-view');
  if (!view) return;
  view.innerHTML = '<div class="home-grid" id="home-grid"></div>';
  const grid = document.getElementById('home-grid');
  if (grid) renderSkeleton(6, grid);

  S.home.pending = S.home.pending || [];
  S.home.convos = S.home.convos || [];
  S.home.lastJob = S.home.lastJob || null;

  const refresh = () => renderHome(
    S.home.pending || [],
    S.home.lastJob || null,
    S.home.convos || []
  );

  // 首页不能被某个慢接口拖死：先渲染缓存/空态，再让各卡片独立回来。
  refresh();

  api('/pending').then(r => {
    if (!r.error) {
      S.home.pending = r.events || [];
      S.home.pendingCount = S.home.pending.length;
      refresh();
    }
  }).catch(() => {});

  api('/jobs?limit=1').then(r => {
    if (!r.error) {
      S.home.lastJob = r.jobs && r.jobs.length ? r.jobs[0] : null;
      refresh();
    }
  }).catch(() => {});

  api('/overview?limit=3').then(r => {
    if (!r.error) {
      S.home.convos = r.conversations || [];
      refresh();
    }
  }).catch(() => {});

  // Relay status (non-blocking)
  api('/relay/status').then(r => {
    if (!r.error) {
      S.home.relayStatus = r;
      const el = document.getElementById('home-relay');
      if (el) el.innerHTML = renderHomeRelay();
    }
  }).catch(() => {});

  // Provider status (cached, non-blocking)
  if (!S.home.ctx) {
    api('/run/context').then(r => {
      if (!r.error) {
        S.home.ctx = r;
        const el = document.getElementById('home-providers');
        if (el) el.innerHTML = renderHomeProviders();
      }
    }).catch(() => {});
  }
}

function renderHome(pending, lastJob, convos) {
  const grid = document.getElementById('home-grid');
  if (!grid) return;

  let html = '';

  // 1. Current Mode
  const currentMode = MODES.find(m => m.id === S.mode);
  html += '<div class="home-card">';
  html += '<div class="home-card-title">当前模式</div>';
  if (currentMode) {
    html += '<div style="font-size:1.1rem;font-weight:700;margin-bottom:4px">' + esc(currentMode.label) + '</div>';
    html += '<div style="font-size:.8rem;color:var(--dim);margin-bottom:14px">' + esc(currentMode.desc) + '</div>';
  }
  html += '<div class="mode-grid">';
  MODES.forEach(m => {
    html += '<div class="mode-btn' + (m.id === S.mode ? ' active' : '') + '" onclick="setMode(\'' + jsStr(m.id) + '\')">' + esc(m.label) + '</div>';
  });
  html += '</div></div>';

  // 2. Pending Asks
  html += '<div class="home-card" id="home-pending-card">';
  html += '<div class="home-card-title">待审批' + (pending.length ? ' <span style="font-size:.72rem;color:var(--yellow)">' + pending.length + '</span>' : '') + '</div>';
  html += '<div class="home-card-body">' + renderHomePending(pending) + '</div>';
  html += '</div>';

  // 3. Phone Connection (Relay Pairing)
  html += '<div class="home-card">';
  html += '<div class="home-card-title">手机连接</div>';
  html += '<div id="home-relay">' + renderHomeRelay() + '</div>';
  html += '</div>';

  // 4. Recent Jobs
  html += '<div class="home-card">';
  html += '<div class="home-card-title">最近任务</div>';
  html += renderHomeJobs(lastJob);
  html += '</div>';

  // 5. Recent Conversations
  html += '<div class="home-card">';
  html += '<div class="home-card-title">最近会话</div>';
  html += renderHomeConvos(convos);
  html += '</div>';

  // 6. Provider Status
  html += '<div class="home-card">';
  html += '<div class="home-card-title">Provider 状态</div>';
  html += '<div id="home-providers">' + renderHomeProviders() + '</div>';
  html += '</div>';

  grid.innerHTML = html;
}

function renderHomePending(events) {
  if (!events || !events.length) {
    return emptyState('暂无待审批', '所有请求已处理');
  }
  let h = '';
  events.slice(0, 3).forEach(e => {
    h += '<div class="pending-card" onclick="jumpToEvent(\'' + jsStr(e.event_id) + '\')">';
    h += '<div class="pending-card-row">';
    h += badge('ask', ACTION_LABELS.ask);
    h += '<span class="pending-card-tool">' + esc(e.tool_name || '-') + '</span>';
    h += '<svg class="pending-card-arrow" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="9 18 15 12 9 6"/></svg>';
    h += '</div>';
    h += renderApprovalReview(e);
    h += '<div class="pending-card-actions" onclick="event.stopPropagation()">';
    h += '<button class="btn btn-sm btn-approve" onclick="homeDecide(\'' + jsStr(e.event_id) + '\',\'allow\')">允许</button>';
    h += '<button class="btn btn-sm btn-reject" onclick="homeDecide(\'' + jsStr(e.event_id) + '\',\'deny\')">拒绝</button>';
    h += '</div></div>';
  });
  if (events.length > 3) {
    h += '<div class="pending-see-all" onclick="jumpToPendingEvents()">查看全部 ' + events.length + ' 条待审批 →</div>';
  }
  return h;
}

function renderHomeJobs(job) {
  if (!job) {
    return emptyState('暂无任务', '在 Run 标签提交任务');
  }
  let h = '<div class="job-item" onclick="switchTab(\'run\');if(typeof viewJob===\'function\')viewJob(\'' + jsStr(job.id) + '\')">';
  h += '<div style="display:flex;align-items:center;gap:8px;margin-bottom:4px">';
  h += renderBridgeJobBadge(job.status);
  h += '<span style="font-weight:600;font-size:.88rem">' + esc(jobKindLabel(job.kind)) + '</span>';
  h += '</div>';
  h += '<div class="job-meta">' + ago(job.created_at) + '</div>';
  h += '</div>';
  return h;
}

function renderHomeConvos(convos) {
  if (!convos || !convos.length) {
    return emptyState('暂无会话', '会话将自动显示');
  }
  let h = '';
  convos.slice(0, 3).forEach(c => {
    const src = (c.title_source && c.title_source !== 'fallback') ? ' <span style="font-size:.62rem;color:var(--dim);background:var(--surface3);padding:1px 5px;border-radius:4px">' + esc(c.title_source) + '</span>' : '';
    h += '<div class="convo-item" onclick="switchTab(\'conv\');if(typeof openConvDetail===\'function\')openConvDetail(\'' + jsStr(c.id) + '\')">';
    h += '<div class="convo-title">' + esc(c.title || '未命名') + src + '</div>';
    h += '<div class="convo-meta">' + esc(AGENTS[c.agent] || c.agent || '') + ' · ' + ago(c.last_seen_at) + '</div>';
    h += '</div>';
  });
  return h;
}

function renderHomeProviders() {
  const ctx = S.home.ctx;
  if (!ctx) {
    return '<div style="font-size:.8rem;color:var(--dim);padding:8px 0">加载中…</div>';
  }
  const avail = ctx.provider_availability || [];
  const map = {};
  avail.forEach(a => { map[a.provider] = a.available; });
  const providers = [
    { key: 'claude_code', label: 'Claude Code' },
    { key: 'kimi_code', label: 'Kimi Code' },
    { key: 'codex_cli', label: 'Codex CLI' }
  ];
  let h = '';
  providers.forEach(p => {
    const available = map[p.key] || false;
    const icon = available
      ? '<span style="color:var(--green)">✓</span>'
      : '<span style="color:var(--red)">✕</span>';
    h += '<div style="display:flex;align-items:center;justify-content:space-between;padding:8px 0;border-bottom:1px solid var(--border);font-size:.85rem">';
    h += '<span>' + esc(p.label) + '</span>';
    h += icon;
    h += '</div>';
  });
  return h;
}

function renderHomeRelay() {
  const rs = S.home.relayStatus;
  if (!rs) {
    return '<div style="font-size:.8rem;color:var(--dim);padding:8px 0">加载中…</div>';
  }
  if (!rs.enabled) {
    return emptyState('未启用远程连接', '配置 relay_url 后可从手机访问');
  }
  if (!rs.connected) {
    return emptyState('正在连接 Relay…', rs.relay_url || '');
  }

  // Connected — show pairing info
  let h = '';
  h += '<div class="relay-status-row">';
  h += '<span class="relay-status-ok">已连接</span>';
  h += '<span class="relay-mobile-url">' + esc(rs.mobile_url || '') + '</span>';
  h += '</div>';

  // Token display (masked by default)
  h += '<div class="relay-token-section">';
  h += '<div class="relay-token-label">Client Token</div>';
  h += '<div id="relay-token-display" class="relay-token-block relay-token-masked">••••••••••••••••••••••••</div>';
  h += '</div>';

  // Action buttons
  h += '<div class="relay-btn-row">';
  h += '<button class="btn btn-sm btn-ghost" onclick="copyRelayUrl()">复制链接</button>';
  h += '<button class="btn btn-sm btn-ghost" onclick="copyRelayToken()">复制 Token</button>';
  h += '<button class="btn btn-sm btn-ghost" onclick="toggleRelayToken()" id="relay-token-toggle-btn">显示 Token</button>';
  h += '</div>';

  return h;
}

function toggleRelayToken() {
  const el = document.getElementById('relay-token-display');
  const btn = document.getElementById('relay-token-toggle-btn');
  if (!el || !btn) return;

  if (el.dataset.visible === '1') {
    el.textContent = '••••••••••••••••••••••••';
    el.dataset.visible = '0';
    el.classList.add('relay-token-masked');
    btn.textContent = '显示 Token';
  } else {
    // Fetch token on first reveal
    if (!S.home.relayPairing) {
      api('/relay/pairing').then(r => {
        if (!r.error) {
          S.home.relayPairing = r;
          el.textContent = r.client_token || '';
          el.dataset.visible = '1';
          el.classList.remove('relay-token-masked');
          btn.textContent = '隐藏 Token';
        } else {
          toast('获取 Token 失败');
        }
      }).catch(() => toast('获取 Token 失败'));
      return;
    }
    el.textContent = S.home.relayPairing.client_token || '';
    el.dataset.visible = '1';
    el.classList.remove('relay-token-masked');
    btn.textContent = '隐藏 Token';
  }
}

function copyRelayUrl() {
  const rs = S.home.relayStatus;
  if (!rs || !rs.mobile_url) { toast('链接不可用'); return; }
  navigator.clipboard.writeText(rs.mobile_url).then(() => toast('已复制链接')).catch(() => toast('复制失败'));
}

function copyRelayToken() {
  if (!S.home.relayPairing) {
    api('/relay/pairing').then(r => {
      if (!r.error) {
        S.home.relayPairing = r;
        navigator.clipboard.writeText(r.client_token).then(() => toast('已复制 Token')).catch(() => toast('复制失败'));
      } else {
        toast('获取 Token 失败');
      }
    }).catch(() => toast('获取 Token 失败'));
    return;
  }
  navigator.clipboard.writeText(S.home.relayPairing.client_token).then(() => toast('已复制 Token')).catch(() => toast('复制失败'));
}

function homeDecide(id, action) {
  api('/decide', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ event_id: id, action: action })
  }).then(r => {
    if (r.error) {
      toast('操作失败: ' + (r.error || '未知错误'));
      return;
    }
    toast(action === 'allow' ? '已允许' : '已拒绝');
    // Reload home to reflect changes
    loadHome();
  }).catch(() => {
    toast('操作失败');
  });
}
