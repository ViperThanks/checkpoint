// job_status.js — Bridge / Relay 共用的任务状态展示原语
//
// 职责：集中维护 job status、completion reason/detail 的中文展示映射。
// 不变量：本文件只做纯展示转换，不读取 DOM，不发请求，不改变 job 状态语义。

(function (root) {
  const STATUS_VIEW = {
    queued: { label: '排队中', relayClass: 'badge-yellow', bridgeType: 'ask' },
    running: { label: '运行中', relayClass: 'badge-blue', bridgeType: 'ask' },
    succeeded: { label: '成功', relayClass: 'badge-green', bridgeType: 'allow' },
    failed: { label: '失败', relayClass: 'badge-red', bridgeType: 'deny' },
    cancelled: { label: '已取消', relayClass: 'badge-gray', bridgeType: 'deny' },
    timeout: { label: '空闲超时', relayClass: 'badge-red', bridgeType: 'deny' },
    observing: { label: '等待返回', relayClass: 'badge-orange', bridgeType: 'ask' },
  };

  const TERMINAL_STATUS = {
    succeeded: true,
    failed: true,
    cancelled: true,
    timeout: true,
  };

  function htmlEscape(value) {
    if (root.escHtml) return root.escHtml(value);
    return String(value == null ? '' : value)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  function jobStatusView(status) {
    return STATUS_VIEW[status] || {
      label: status || '未知',
      relayClass: 'badge-gray',
      bridgeType: '',
    };
  }

  function terminalJobStatus(status) {
    return !!TERMINAL_STATUS[status];
  }

  function renderJobBadge(status) {
    const view = jobStatusView(status);
    return '<span class="badge ' + htmlEscape(view.relayClass) + '">' + htmlEscape(view.label) + '</span>';
  }

  function renderBridgeJobBadge(status) {
    const view = jobStatusView(status);
    const cls = view.bridgeType ? ' badge-' + htmlEscape(view.bridgeType) : '';
    return '<span class="badge' + cls + '">' + htmlEscape(view.label) + '</span>';
  }

  function humanCompletedReason(reason, opts) {
    if (!reason) return '';
    opts = opts || {};
    const prefix = opts.completedPrefix ? '已完成：' : '';
    if (reason === 'stop_hook') return prefix + 'stop hook';
    if (reason === 'process_exit') return prefix + '进程退出';
    if (reason === 'scanner_timeout' || reason === 'timeout_killed') return '超时';
    if (reason === 'process_exit_nonzero') return '进程异常';
    return reason;
  }

  function cleanCompletionName(value) {
    if (!value) return '';
    return String(value).replace(/^"|"$/g, '');
  }

  function humanCompletionDetail(completion, relTimeFn) {
    if (!completion) return '';
    const formatRelTime = relTimeFn || root.relTime || function (value) { return String(value || ''); };
    const parts = [];
    const signal = cleanCompletionName(completion.signal);
    const authority = cleanCompletionName(completion.authority);
    if (signal) parts.push(signal);
    if (authority) parts.push(authority);
    if (completion.last_activity_at) parts.push('activity ' + formatRelTime(completion.last_activity_at));
    if (completion.hard_deadline_at) parts.push('deadline ' + formatRelTime(completion.hard_deadline_at));
    return parts.join(' · ');
  }

  root.jobStatusView = jobStatusView;
  root.terminalJobStatus = terminalJobStatus;
  root.renderJobBadge = renderJobBadge;
  root.renderBridgeJobBadge = renderBridgeJobBadge;
  root.humanCompletedReason = humanCompletedReason;
  root.humanCompletionDetail = humanCompletionDetail;

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = {
      jobStatusView,
      terminalJobStatus,
      renderJobBadge,
      renderBridgeJobBadge,
      humanCompletedReason,
      humanCompletionDetail,
    };
  }
})(typeof globalThis !== 'undefined' ? globalThis : this);
