// crates/bridge/src/ui/tabs/events.js
// Events tab: filter sidebar, event list, detail panel.

function eventCardHash(e) {
  return [
    e.action,
    e.rule_id || '',
    e.tool_name || '',
    e.file_path || '',
    e.agent || '',
    e.feedback?.verdict || '',
    e.feedback?.note || '',
    e.conversation?.conversation_id || ''
  ].join('\x01');
}

function groupCardHash(g) {
  return [
    g.count,
    g.first_timestamp,
    g.last_timestamp,
    g.agent || '',
    g.tool_name || '',
    g.action,
    g.rule_id || '',
    g.sample_file_path || ''
  ].join('\x01');
}

function buildCard(e) {
  const bc = e.action === 'allow' ? 'allow' : e.action === 'deny' ? 'deny' : 'ask';
  const ag = AGENTS[e.agent] || e.agent || '';
  const c = cmd(e.raw_payload);
  const fp = e.file_path || '';
  const fb = e.feedback;

  const card = document.createElement('div');
  card.className = 'ecard' + (S.selectedId === e.event_id ? ' selected' : '');
  card.dataset.eid = e.event_id;
  card.dataset.hash = eventCardHash(e);

  const top = document.createElement('div');
  top.className = 'ecard-top';

  const badgeEl = document.createElement('span');
  badgeEl.className = 'badge badge-' + bc;
  badgeEl.textContent = ACTION_LABELS[e.action] || e.action;

  const tool = document.createElement('span');
  tool.className = 'ecard-tool';
  tool.textContent = e.tool_name || '-';

  top.appendChild(badgeEl);
  top.appendChild(tool);

  if (ag) {
    const al = document.createElement('span');
    al.className = 'ecard-agent';
    al.textContent = ' ' + ag;
    tool.appendChild(al);
  }

  if (fb && fb.verdict) {
    const dot = document.createElement('span');
    dot.className = 'ecard-fb ecard-fb-' + fb.verdict;
    dot.style.cssText = 'display:inline-block;width:6px;height:6px;border-radius:50%;margin-left:4px;vertical-align:middle;background:var(--green)';
    tool.appendChild(dot);
  }

  const meta = document.createElement('div');
  meta.className = 'ecard-meta';
  const parts = [];
  if (fp) parts.push(fp);
  if (e.rule_id) parts.push(S.ruleMap[e.rule_id] || e.rule_id);
  if (c) parts.push(c.substring(0, 50));
  parts.push(ago(e.timestamp));
  meta.textContent = parts.join(' · ');
  card.appendChild(top);
  card.appendChild(meta);

  if (e.conversation && (e.conversation.title || e.conversation.conversation_id)) {
    const conv = document.createElement('div');
    conv.className = 'ecard-conv';
    conv.innerHTML = esc(e.conversation.title || '未命名') + ' <span class="ecard-conv-id">' + shortId(e.conversation.conversation_id || '') + '</span>';
    conv.onclick = (ev) => {
      ev.stopPropagation();
      switchTab('conv');
      openConvDetail(e.conversation.conversation_db_id || e.conversation.conversation_id);
    };
    card.appendChild(conv);
  }

  card.onclick = () => selectEvent(e.event_id);
  return card;
}

function buildGroupCard(g) {
  const ag = AGENTS[g.agent] || g.agent || '';

  const card = document.createElement('div');
  card.className = 'ecard group-card' + (S.selectedId === g.group_id ? ' selected' : '');
  card.dataset.gid = g.group_id;
  card.dataset.hash = groupCardHash(g);

  const top = document.createElement('div');
  top.className = 'ecard-top';

  const badge = document.createElement('span');
  badge.className = 'badge';
  badge.style.cssText = 'background:var(--surface3);color:var(--dim)';
  badge.textContent = '×' + g.count;

  const tool = document.createElement('span');
  tool.className = 'ecard-tool';
  tool.textContent = g.tool_name || '-';

  top.appendChild(badge);
  top.appendChild(tool);

  if (ag) {
    const al = document.createElement('span');
    al.className = 'ecard-agent';
    al.textContent = ag;
    tool.appendChild(al);
  }

  const meta = document.createElement('div');
  meta.className = 'ecard-meta';
  const parts = [];
  if (g.rule_id) parts.push(S.ruleMap[g.rule_id] || g.rule_id);
  if (g.first_timestamp) parts.push(ago(g.first_timestamp));
  meta.textContent = parts.join(' · ');

  const bottom = document.createElement('div');
  bottom.className = 'ecard-meta';
  bottom.style.marginTop = '2px';
  bottom.textContent = g.count + ' 条事件 · ' + (g.tool_name || '-') + ' · ' + (ACTION_LABELS[g.action] || g.action);

  card.appendChild(top);
  card.appendChild(meta);
  card.appendChild(bottom);
  card.onclick = () => selectEvent(g.group_id);
  return card;
}

function selectEvent(id) {
  S.selectedId = id;
  document.querySelectorAll('.ecard').forEach(el => el.classList.toggle('selected', el.dataset.eid === id || el.dataset.gid === id));
  renderDetail(id);
}

function renderDetail(id) {
  const ev = (S.events || []).find(e => !e.is_group && e.event_id === id);
  if (ev) { renderEventDetail(ev); return; }
  const gr = (S.events || []).find(e => e.is_group && e.group_id === id);
  if (gr) { renderGroupDetail(gr); return; }
}

function renderEventDetailHTML(ev) {
  const ag = AGENTS[ev.agent] || ev.agent || '';
  const c = cmd(ev.raw_payload);
  const fb = ev.feedback;

  let h = '<div class="det-head">' +
    badge(ev.action === 'allow' ? 'allow' : ev.action === 'deny' ? 'deny' : 'ask', ACTION_LABELS[ev.action] || ev.action) +
    '<span class="det-tool">' + esc(ev.tool_name || '-') + '</span>';
  if (ag) h += '<span class="ecard-agent">' + esc(ag) + '</span>';
  h += '</div>';

  h += '<div class="det-field"><div class="det-label">规则</div><div class="det-value">' + esc(ev.rule_id ? (S.ruleMap[ev.rule_id] || ev.rule_id) : '-') + '</div></div>';
  if (ev.note) h += '<div class="det-field"><div class="det-label">备注</div><div class="det-value">' + esc(ev.note) + '</div></div>';
  if (ev.file_path) h += '<div class="det-field"><div class="det-label">文件</div><div class="det-value">' + esc(ev.file_path) + '</div></div>';
  if (c) h += '<div class="det-field"><div class="det-label">命令</div><div class="det-value"><code class="det-command">' + esc(c) + '</code></div></div>';
  h += '<div class="det-field"><div class="det-label">时间</div><div class="det-value">' + (ev.timestamp ? ev.timestamp.replace('T', ' ').substring(0, 19) + ' (' + ago(ev.timestamp) + ')' : '-') + '</div></div>';
  if (ev.agent) h += '<div class="det-field"><div class="det-label">代理</div><div class="det-value">' + esc(ag || ev.agent) + '</div></div>';

  h += '<div class="verdict-bar">';
  ['useful', 'noisy', 'wrong', 'unsure'].forEach(v => {
    const sel = fb && fb.verdict === v;
    h += '<button class="verdict-btn' + (sel ? ' sel-' + v : '') + '" onclick="sendFb(\'' + jsStr(ev.event_id) + '\',\'' + v + '\')">' + (VERDICT_LABELS[v] || v) + '</button>';
  });
  h += '</div>';

  const draft = (ev.event_id in S.draftNotes) ? S.draftNotes[ev.event_id] : (fb && fb.note ? fb.note : '');
  const hasContent = draft.length > 0 || (fb && fb.note);
  const isOpen = S.noteOpen[ev.event_id] !== undefined ? S.noteOpen[ev.event_id] : !!(fb && fb.note);

  h += '<button class="note-toggle" onclick="toggleNote(\'' + jsStr(ev.event_id) + '\')">' + (hasContent ? '编辑备注' : '+ 添加备注') + '</button>';
  h += '<div class="note-area' + (isOpen ? ' open' : '') + '" id="note-area-' + jsStr(ev.event_id) + '">';
  h += '<textarea id="note-ta-' + jsStr(ev.event_id) + '" oninput="saveDraft(\'' + jsStr(ev.event_id) + '\')">' + esc(draft) + '</textarea>';
  h += '<button onclick="saveNote(\'' + jsStr(ev.event_id) + '\')">保存</button></div>';

  if (ev.raw_payload) {
    const pretty = tryPrettyJSON(ev.raw_payload);
    const isLong = pretty.length > 300;
    h += '<div class="det-field" style="margin-top:12px"><div class="det-label">Payload</div><div>';
    if (isLong) {
      h += '<button class="payload-toggle" onclick="togglePayload(this)">展开</button><pre class="det-payload collapsed">' + esc(pretty) + '</pre>';
    } else {
      h += '<pre class="det-payload">' + esc(pretty) + '</pre>';
    }
    h += '</div></div>';
  }

  return h;
}

function renderEventDetail(ev) {
  const hash = eventCardHash(ev);
  if (S.detailRenderedId === ev.event_id && S.detailHash === hash) return;
  openDetail(renderEventDetailHTML(ev));
  S.detailRenderedId = ev.event_id;
  S.detailHash = hash;
}

function renderGroupDetail(gr) {
  const hash = groupCardHash(gr);
  if (S.detailRenderedId === gr.group_id && S.detailHash === hash) return;

  const ag = AGENTS[gr.agent] || gr.agent || '';
  let h = '<div class="det-head"><span class="badge" style="background:var(--surface3);color:var(--dim)">×' + gr.count + '</span>' +
    '<span class="det-tool">' + esc(gr.tool_name || '-') + '</span>';
  if (ag) h += '<span class="ecard-agent">' + esc(ag) + '</span>';
  h += '</div>';

  h += '<div class="det-field"><div class="det-label">操作</div><div class="det-value">' + esc(ACTION_LABELS[gr.action] || gr.action) + '</div></div>';
  h += '<div class="det-field"><div class="det-label">规则</div><div class="det-value">' + esc(gr.rule_id ? (S.ruleMap[gr.rule_id] || gr.rule_id) : '-') + '</div></div>';
  h += '<div class="det-field"><div class="det-label">事件数</div><div class="det-value">' + gr.count + ' 条</div></div>';
  if (gr.sample_file_path) h += '<div class="det-field"><div class="det-label">示例文件</div><div class="det-value">' + esc(gr.sample_file_path) + '</div></div>';
  if (gr.event_ids && gr.event_ids.length) {
    h += '<div class="det-field"><div class="det-label">事件 ID</div><div>';
    gr.event_ids.slice(0, 20).forEach(id => {
      h += '<span class="id-chip" onclick="fetchEventDetail(\'' + jsStr(id) + '\')">' + esc(id.slice(0, 8)) + '...</span>';
    });
    if (gr.event_ids.length > 20) h += '<span class="id-chip">+ ' + (gr.event_ids.length - 20) + '</span>';
    h += '</div></div>';
  }

  openDetail(h);
  S.detailRenderedId = gr.group_id;
  S.detailHash = hash;
}

function fetchEventDetail(eid) {
  api('/events/' + eid).then(r => {
    if (r.error) return;
    openDetail(renderEventDetailHTML(r));
  });
}

function toggleNote(eid) {
  const el = document.getElementById('note-area-' + eid);
  if (!el) return;
  S.noteOpen[eid] = el.classList.toggle('open');
}

function saveDraft(eid) {
  const ta = document.getElementById('note-ta-' + eid);
  if (ta) S.draftNotes[eid] = ta.value;
}

function saveNote(eid) {
  const note = S.draftNotes[eid] || '';
  const ev = (S.events || []).find(e => e.event_id === eid);
  const verdict = ev && ev.feedback ? ev.feedback.verdict : '';
  api('/events/' + eid + '/feedback', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ verdict: verdict || 'unsure', note })
  }).then(r => {
    if (!r.error) {
      toast('备注已保存');
      loadEvents();
      loadPending();
    }
  });
}

function sendFb(eid, v) {
  const note = S.draftNotes[eid] || '';
  api('/events/' + eid + '/feedback', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ verdict: v, note })
  }).then(r => {
    if (!r.error) {
      toast('评价已保存');
      loadEvents();
      loadPending();
    }
  });
}

function togglePayload(btn) {
  const pre = btn.nextElementSibling;
  if (pre.classList.contains('collapsed')) {
    pre.classList.remove('collapsed');
    btn.textContent = '收起';
  } else {
    pre.classList.add('collapsed');
    btn.textContent = '展开';
  }
}

function getFilterQS() {
  const f = S.filters;
  let qs = 'limit=' + PAGE + '&offset=' + S.offset;
  if (f.action) qs += '&action=' + encodeURIComponent(f.action);
  if (f.agent) qs += '&agent=' + encodeURIComponent(f.agent);
  if (f.verdict) qs += '&verdict=' + encodeURIComponent(f.verdict);
  if (S.groupMode === 'compact') qs += '&group=compact';
  return qs;
}

function renderEventList() {
  const list = document.getElementById('events-list');
  list.querySelectorAll('.skeleton-card').forEach(el => el.remove());

  const existing = Array.from(list.querySelectorAll('.ecard'));
  const existingMap = new Map();
  existing.forEach(el => existingMap.set(el.dataset.gid || el.dataset.eid, el));

  const newIds = new Set(S.events.map(e => e.is_group ? e.group_id : e.event_id));
  existing.forEach(el => {
    const key = el.dataset.gid || el.dataset.eid;
    if (!newIds.has(key)) el.remove();
  });

  if (!S.events.length) {
    if (!list.querySelector('.empty-state')) {
      list.innerHTML = emptyState('暂无事件', '代理执行操作后事件会出现在这里');
    }
    return;
  }

  const es = list.querySelector('.empty-state');
  if (es) es.remove();

  let prevEl = null;
  S.events.forEach(e => {
    const id = e.is_group ? e.group_id : e.event_id;
    let card = existingMap.get(id);
    const hash = e.is_group ? groupCardHash(e) : eventCardHash(e);
    if (!card || card.dataset.hash !== hash) {
      const nc = e.is_group ? buildGroupCard(e) : buildCard(e);
      if (card) {
        list.replaceChild(nc, card);
      } else if (prevEl) {
        prevEl.after(nc);
      } else {
        list.prepend(nc);
      }
      card = nc;
    } else {
      card.classList.toggle('selected', S.selectedId === id);
    }
    prevEl = card;
  });
}

function loadEvents(opts) {
  opts = opts || {};
  if (opts.showLoading && S.firstLoad) {
    renderSkeleton(6, 'events-list');
  }
  document.getElementById('events-list').classList.add('loading');
  api('/events?' + getFilterQS()).then(r => {
    document.getElementById('events-list').classList.remove('loading');
    if (r.error) { return; }
    S.total = r.total || 0;
    S.events = r.events || [];
    S.firstLoad = false;
    renderEventList();
    updatePager();
    if (S.selectedId) renderDetail(S.selectedId);
    if (opts.onDone) opts.onDone();
  });
}

function loadPending() {
  api('/pending').then(r => {
    if (r.error) return;
    const box = document.getElementById('pending-box');
    if (!box) return;
    // Store count for consistent badge display
    S.home.pendingCount = r.count || (r.events ? r.events.length : 0);
    const badge = document.getElementById('pending-count');
    if (badge) badge.textContent = S.home.pendingCount > 0 ? S.home.pendingCount : '';

    if (!r.events || !r.events.length) {
      box.innerHTML = '<div style="color:var(--dimmer);font-size:.78rem;padding:8px 0;text-align:center">暂无待审批</div>';
      return;
    }

    box.innerHTML = '';
    r.events.forEach(e => {
      const item = document.createElement('div');
      item.style.cssText = 'padding:10px 0;border-bottom:1px solid var(--border)';

      const top = document.createElement('div');
      top.style.cssText = 'display:flex;align-items:center;gap:6px';

      const b = document.createElement('span');
      b.className = 'badge badge-ask';
      b.textContent = '待审批';

      const tool = document.createElement('span');
      tool.style.cssText = 'font-size:.78rem;font-weight:600;flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap';
      tool.textContent = e.tool_name || '-';

      top.appendChild(b);
      top.appendChild(tool);

      const meta = document.createElement('div');
      meta.style.cssText = 'font-size:.7rem;color:var(--dim);margin-top:3px';
      const reviewHtml = renderApprovalReview(e);
      if (reviewHtml) {
        meta.innerHTML = reviewHtml;
      } else {
        meta.textContent = (e.file_path ? e.file_path + ' · ' : '') + (e.rule_id ? (S.ruleMap[e.rule_id] || e.rule_id) : '') + ' · ' + ago(e.timestamp);
      }

      const act = document.createElement('div');
      act.style.cssText = 'display:flex;gap:6px;margin-top:6px';

      const ok = document.createElement('button');
      ok.className = 'btn btn-approve btn-sm';
      ok.textContent = '允许';
      ok.onclick = () => decide(e.event_id, 'allow');

      const no = document.createElement('button');
      no.className = 'btn btn-reject btn-sm';
      no.textContent = '拒绝';
      no.onclick = () => decide(e.event_id, 'deny');

      act.appendChild(ok);
      act.appendChild(no);

      item.appendChild(top);
      item.appendChild(meta);
      item.appendChild(act);
      box.appendChild(item);
    });
  });
}

function decide(id, a) {
  api('/decide', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ event_id: id, action: a })
  }).then(r => {
    if (!r.error) {
      toast(a === 'allow' ? '已允许' : '已拒绝');
      loadPending();
      loadEvents();
    }
  });
}

function updatePager() {
  const start = S.offset + 1;
  const end = Math.min(S.offset + PAGE, S.total);
  document.getElementById('pager-info').textContent = S.total > 0 ? start + '-' + end + ' / ' + S.total : '0 条';
  document.getElementById('btn-prev').disabled = S.offset <= 0;
  document.getElementById('btn-next').disabled = end >= S.total;
}

function pagePrev() {
  S.offset = Math.max(0, S.offset - PAGE);
  loadEvents();
}

function pageNext() {
  if (S.offset + PAGE < S.total) {
    S.offset += PAGE;
    loadEvents();
  }
}

function resetAndLoad() {
  S.offset = 0;
  loadEvents();
}

function syncFilters() {
  S.filters.action = document.getElementById('s-action').value;
  S.filters.agent = document.getElementById('s-agent').value;
  S.filters.verdict = document.getElementById('s-verdict').value;
  resetAndLoad();
}
