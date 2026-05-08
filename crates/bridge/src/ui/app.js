// ===== Constants =====
const MODES=[
  {id:'observer',label:'Observer',desc:'只观察不拦截'},
  {id:'autonomous',label:'Auto',desc:'低敏感度自动通过'},
  {id:'guard',label:'Guard',desc:'标准保护模式'},
  {id:'paranoid',label:'Paranoid',desc:'最高安全级别'}
];
const AGENTS={claude_code:'Claude Code',codex_cli:'Codex CLI',kimi_code:'Kimi Code',gemini_cli:'Gemini CLI',z_code:'Z',opencode:'OpenCode'};
const ACTION_LABELS={allow:'允许',deny:'拒绝',ask:'待审批'};
const STATUS_LABELS={succeeded:'成功',failed:'失败',cancelled:'已取消',timeout:'空闲超时',observing:'等待返回',running:'运行中',queued:'排队中'};
const VERDICT_LABELS={useful:'有用',noisy:'噪音',wrong:'错误',unsure:'不确定'};
const JOB_KINDS={agent_aspect_status:'状态检查',git_status:'Git 状态',cargo_test:'Cargo 测试',smoke_test:'冒烟测试',agent_aspect_mode:'模式设置',agent_prompt:'代理提示词'}; // fallback, overridden by /job-kinds
function jobKindLabel(k){return S.jobKinds[k]||JOB_KINDS[k]||k}
const PAGE=20;
const SKELETON_HTML='<div class="skeleton-card"><div class="skeleton-row" style="width:45%"></div><div class="skeleton-row" style="width:80%"></div><div class="skeleton-row" style="width:60%"></div></div>';

// cleanAgentLogChunk — 来自 shared_ui/view_model.js
// escHtml/esc, jsStr, formatTime/formatMsgTime, relTime/ago, shortId — 来自 shared_ui/view_model.js
// renderMd — 来自 shared_ui/render.js
// api — 来自 shared_ui/api_client.js
// runtimeAlertCard, runtimeHealthBadge — 来自 shared_ui/runtime_health.js

// ===== Global State =====
const S={
  token:localStorage.getItem('agent_aspect_session_token')||'',
  mode:'',total:0,offset:0,
  selectedId:null,detailRenderedId:null,detailHash:'',
  draftNotes:{},noteOpen:{},
  filters:{action:'',agent:'',verdict:''},
  firstLoad:true,events:[],groupMode:'compact',
  tab:'home',
  conv:{offset:0,total:0,agentFilter:'',detailCid:null,subTab:'chat',messagesLoaded:false},
  home:{pendingCount:0,lastJob:null,convos:[],activeJobConvId:null},
  ruleMap:{},jobKinds:{},
  hooks:null
};
let timer=null;

// ===== Bridge-specific API (returns parsed JSON) =====
function api(p,o){
  const h={};
  if(S.token)h['Authorization']='Bearer '+S.token;
  return fetch(p,{...o,headers:{...h,...(o||{}).headers}})
    .then(r=>r.text().then(t=>{try{return JSON.parse(t)}catch(e){return{error:'invalid response'}}}).then(j=>{if(!r.ok)throw Object.assign(new Error(j.error||'request failed'),{status:r.status});return j;}))
    .catch(e=>{console.warn('API error:',p,e.message||e);return{error:e.message||'network error'};});
}
function cmd(rp){
  try{const j=JSON.parse(rp);return j.command||j.tool_input?.command||null}catch(e){return null}
}
function tryPrettyJSON(s){try{return JSON.stringify(JSON.parse(s),null,2)}catch(e){return s}}

// ===== Navigation helpers =====
function jumpToEvent(eventId) {
  S.offset = 0;
  S.selectedId = eventId;
  S.filters.action = 'ask';
  const sa = document.getElementById('s-action');
  if (sa) sa.value = 'ask';
  switchTab('events');
}
function jumpToPendingEvents() {
  S.offset = 0;
  S.filters.action = 'ask';
  const sa = document.getElementById('s-action');
  if (sa) sa.value = 'ask';
  switchTab('events');
}
function scrollToSelectedEvent() {
  // Try scrolling to the selected card in the event list
  const el = document.querySelector('.ecard.selected');
  if (el) {
    el.scrollIntoView({ behavior: 'smooth', block: 'center' });
    return;
  }
  // If not found in current page, try fetching the event directly and showing in detail panel
  if (S.selectedId && typeof fetchEventDetail === 'function') {
    fetchEventDetail(S.selectedId);
  }
}

// toast — 来自 shared_ui/view_model.js

// ===== Login =====
function doLogin(){
  const username=document.getElementById('username-input').value.trim();
  const password=document.getElementById('password-input').value;
  const errEl=document.getElementById('login-error');
  const btn=document.getElementById('login-btn');
  if(!username||!password){errEl.textContent='请输入用户名和密码';errEl.style.display='';return}
  errEl.style.display='none';btn.textContent='登录中...';btn.disabled=true;
  fetch('/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({username,password})})
    .then(r=>r.json().then(j=>({ok:r.ok,data:j})))
    .then(({ok,data})=>{
      btn.textContent='登录';btn.disabled=false;
      if(!ok||!data.token){errEl.textContent=data.error||'登录失败';errEl.style.display='';return}
      S.token=data.token;
      localStorage.setItem('agent_aspect_session_token',S.token);
      errEl.style.display='none';
      init();
    })
    .catch(()=>{
      btn.textContent='登录';btn.disabled=false;
      errEl.textContent='网络错误';errEl.style.display='';
    });
}
function doLogout(){
  localStorage.removeItem('agent_aspect_session_token');S.token='';stop();
  document.getElementById('password-input').value='';
}

function showChangePassword(){
  document.getElementById('chpwd-modal').classList.remove('hidden');
  document.getElementById('chpwd-old').value='';
  document.getElementById('chpwd-new').value='';
  document.getElementById('chpwd-confirm').value='';
  var e=document.getElementById('chpwd-error');e.style.display='none';e.textContent='';
}
function closeChangePassword(){
  document.getElementById('chpwd-modal').classList.add('hidden');
}
function doChangePassword(){
  var oldPwd=document.getElementById('chpwd-old').value;
  var newPwd=document.getElementById('chpwd-new').value;
  var confirmPwd=document.getElementById('chpwd-confirm').value;
  var errEl=document.getElementById('chpwd-error');
  if(newPwd.length<12){errEl.textContent='新密码至少 12 个字符';errEl.style.display='block';return;}
  if(newPwd!==confirmPwd){errEl.textContent='两次输入的新密码不一致';errEl.style.display='block';return;}
  errEl.style.display='none';
  api('/password/change',{method:'POST',body:JSON.stringify({old_password:oldPwd,new_password:newPwd})})
    .then(function(r){
      if(r.error){errEl.textContent=r.error;errEl.style.display='block';return;}
      closeChangePassword();
      doLogout();
    });
}

// ===== App Shell =====
function showApp(on){
  document.getElementById('token-overlay').classList.toggle('hidden',on);
  document.getElementById('app').classList.toggle('hidden',!on);
}

// ===== Mode =====
function renderModes(){
  // Header pills (mobile hidden, shown via CSS)
  const c=document.getElementById('header-mode-pills');
  if(c){c.innerHTML=MODES.map(m=>'<div class="mode-btn'+(m.id===S.mode?' active':'')+'" onclick="setMode(\''+m.id+'\')">'+m.label+'</div>').join('');}
  // Sidebar indicator
  const sm=document.getElementById('sidebar-mode');
  if(sm){const m=MODES.find(x=>x.id===S.mode);sm.textContent=m?m.label:S.mode;}
}
function setMode(m){
  api('/mode',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mode:m})})
    .then(r=>{if(!r.error){S.mode=m;renderModes();toast('已切换为 '+m);if(typeof renderHomeMode==='function')renderHomeMode();}});
}

// ===== Tabs =====
function switchTab(tab){
  S.tab=tab;
  // Update sidebar
  ['home','conv','events','run','workflows','hooks'].forEach(t=>{
    const el=document.getElementById('nav-'+t);if(el)el.classList.toggle('active',tab===t);
  });
  // Update mobile tab bar
  ['home','conv','events','run','workflows','hooks'].forEach(t=>{
    const el=document.getElementById('tab-'+t);if(el)el.classList.toggle('active',tab===t);
  });
  // Show/hide views
  ['home','conv','events','run','workflows','hooks'].forEach(t=>{
    const el=document.getElementById(t+'-view');if(el)el.classList.toggle('hidden',tab!==t);
  });
  // Hide detail panel when switching tabs
  closeDetail();
  // Load tab data
  if(tab==='home'&&typeof loadHome==='function')loadHome();
  else if(tab==='conv'&&typeof loadConvList==='function')loadConvList();
  else if(tab==='events'&&typeof loadEvents==='function'){
    const jump=S.selectedId&&S.selectedId!==null;
    loadEvents({showLoading:S.firstLoad,onDone:jump?scrollToSelectedEvent:null});
  }
  else if(tab==='run'&&typeof loadRunContext==='function'){loadRunContext();loadJobHistory();}
  else if(tab==='workflows'&&typeof loadWorkflows==='function'){loadWorkflows();}
  else if(tab==='hooks'&&typeof loadHooks==='function'){loadHooks();}
}

// ===== Detail Panel =====
function openDetail(html){
  const panel=document.getElementById('detail-panel');
  const scroll=document.getElementById('detail-scroll');
  scroll.innerHTML=html||'';
  panel.classList.remove('hidden');
  panel.classList.add('open');
}
function closeDetail(){
  const panel=document.getElementById('detail-panel');
  if(panel){
    panel.classList.remove('open');
    if(window.innerWidth<768){
      setTimeout(()=>panel.classList.add('hidden'),300);
    } else {
      panel.classList.add('hidden');
    }
  }
  // Also close mobile conv detail
  const cdp=document.getElementById('conv-detail-panel');
  if(cdp){cdp.classList.remove('mobile-open');}
  // Also close mobile events sidebar
  const esb=document.getElementById('events-sidebar');
  if(esb){esb.classList.remove('mobile-open');}
}
function toggleEventsSidebar(){
  const esb=document.getElementById('events-sidebar');
  if(esb)esb.classList.toggle('mobile-open');
}

// ===== SSE =====
function shouldPauseRefresh(){
  const dp=document.getElementById('detail-panel');
  if(dp&&dp.classList.contains('open'))return true;
  if(document.activeElement&&document.activeElement.tagName==='TEXTAREA')return true;
  const to=document.getElementById('token-overlay');
  if(to&&!to.classList.contains('hidden'))return true;
  const esb=document.getElementById('events-sidebar');
  if(esb&&esb.classList.contains('mobile-open'))return true;
  return false;
}
function stop(){
  showApp(false);
  if(timer){clearInterval(timer);timer=null}
  if(window._sse){window._sse.close();window._sse=null}
}
function startSSE(){
  if(window._sse)window._sse.close();
  const es=new EventSource('/stream?token='+encodeURIComponent(S.token));window._sse=es;
  es.addEventListener('pending_ask',()=>{if(!shouldPauseRefresh()&&typeof loadPending==='function')loadPending()});
  es.addEventListener('decision',()=>{if(!shouldPauseRefresh()){if(typeof loadPending==='function')loadPending();if(typeof loadEvents==='function')loadEvents();}});
  es.addEventListener('job_status',function(e){
    if(shouldPauseRefresh())return;
    var payload;
    try{payload=JSON.parse(e.data)}catch(err){payload={job_id:e.data}}
    S.lastJobStatus=payload;
    // Clear running indicator on terminal status
    if(payload.status&&(payload.status==='succeeded'||payload.status==='failed'||payload.status==='cancelled')){
      S.home.activeJobConvId=null;
    }
    if(typeof loadJobHistory==='function')loadJobHistory();
    if(window.RS&&RS.activeJobId){
      if(payload.status&&typeof refreshActiveJobFromSSE==='function'){
        refreshActiveJobFromSSE(payload);
      }else if(typeof refreshActiveJob==='function'){
        refreshActiveJob();
      }
    }
    // Refresh conv list to clear running badges
    if(S.tab==='conv'&&typeof loadConvList==='function'&&!S.conv.detailCid)loadConvList();
  });
  es.addEventListener('job_log',()=>{if(!shouldPauseRefresh()){if(typeof refreshActiveJob==='function'&&window.RS&&RS.activeJobId)refreshActiveJob();}});
  es.addEventListener('mode',(e)=>{S.mode=e.data;renderModes();if(typeof renderHomeMode==='function')renderHomeMode();});
  es.addEventListener('conversation_update',function(){
    if(shouldPauseRefresh())return;
    // Refresh conv list if on conv tab without detail open
    if(S.tab==='conv'&&typeof loadConvList==='function'&&!S.conv.detailCid)loadConvList();
    // Refresh active conversation detail
    if(S.conv.detailCid&&typeof loadConvDetail==='function')loadConvDetail();
    // Refresh home convos
    if(S.tab==='home'&&typeof loadHome==='function')loadHome();
  });
  es.addEventListener('workflow_status',function(e){
    if(S.tab==='workflows'&&typeof loadWorkflowList==='function')loadWorkflowList();
    if(S.tab==='workflows'&&WFS.selected&&typeof selectWorkflow==='function')selectWorkflow(WFS.selected.id);
  });
  es.addEventListener('workflow_step_status',function(){
    if(S.tab==='workflows'&&WFS.selected&&typeof selectWorkflow==='function')selectWorkflow(WFS.selected.id);
  });
  es.addEventListener('hook_config',function(){
    if(S.tab==='hooks'&&typeof loadHooks==='function')loadHooks();
  });
  es.onerror=()=>{
    es.close();window._sse=null;
    if(timer)clearInterval(timer);
    timer=setInterval(()=>{
      if(shouldPauseRefresh())return;
      if(typeof loadPending==='function')loadPending();
      if(S.tab==='home'&&typeof loadHome==='function')loadHome();
      else if(S.tab==='events'&&typeof loadEvents==='function')loadEvents();
      else if(S.tab==='conv'&&typeof loadConvList==='function'&&(!S.conv.detailCid))loadConvList();
      else if(S.tab==='workflows'&&typeof loadWorkflowList==='function')loadWorkflowList();
    },30000);
    setTimeout(()=>{if(S.token&&!window._sse)startSSE()},30000);
  };
}

// ===== Init =====
function init(){
  setTheme(getTheme());
  // Display version in sidebar
  var verEl=document.getElementById('sidebar-version');
  if(verEl&&typeof __BUILD_VERSION__!=='undefined'){verEl.textContent='v'+__BUILD_VERSION__;verEl.style.cssText='font-size:11px;color:var(--dimmer);text-align:center;margin-top:6px';}
  if(!S.token){showApp(false);return}
  api('/mode').then(r=>{
    if(r.error){stop();return}
    S.mode=r.mode;renderModes();showApp(true);switchTab(S.tab);
    api('/rules').then(rr=>{if(!rr.error&&rr.rules){rr.rules.forEach(x=>{S.ruleMap[x.id]=x.description})}});
    api('/job-kinds').then(jr=>{if(!jr.error&&jr.kinds){jr.kinds.forEach(x=>{S.jobKinds[x.id]=x.label})}});
    if(typeof loadPending==='function')loadPending();
    if(timer)clearInterval(timer);startSSE();
  }).catch(()=>stop());
}

// Enter key triggers login
document.getElementById('password-input').addEventListener('keydown',function(e){if(e.key==='Enter')doLogin()});
document.getElementById('username-input').addEventListener('keydown',function(e){if(e.key==='Enter')document.getElementById('password-input').focus()});
init();
