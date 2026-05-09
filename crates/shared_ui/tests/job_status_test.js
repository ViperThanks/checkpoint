// job_status_test.js — 共享 job_status 模块回归测试
//
// 职责：锁定 Bridge / Relay 共同依赖的任务状态和完成原因展示语义。

const {
  jobStatusView,
  terminalJobStatus,
  renderJobBadge,
  renderBridgeJobBadge,
  humanCompletedReason,
  humanCompletionDetail,
} = require('../job_status.js');

let passed = 0;
let failed = 0;

function assert(condition, label) {
  if (condition) passed++;
  else { failed++; console.error('  FAIL: ' + label); }
}

function assertEqual(actual, expected, label) {
  if (actual === expected) passed++;
  else {
    failed++;
    console.error('  FAIL: ' + label);
    console.error('    expected: ' + expected);
    console.error('    actual:   ' + actual);
  }
}

function assertContains(value, part, label) {
  if (String(value).indexOf(part) >= 0) passed++;
  else {
    failed++;
    console.error('  FAIL: ' + label);
    console.error('    expected to contain: ' + part);
    console.error('    actual: ' + value);
  }
}

console.log('jobStatusView');

(function test_known_statuses() {
  assertEqual(jobStatusView('queued').label, '排队中', 'queued label');
  assertEqual(jobStatusView('succeeded').bridgeType, 'allow', 'succeeded bridge type');
  assertEqual(jobStatusView('failed').relayClass, 'badge-red', 'failed relay class');
  assertEqual(jobStatusView('observing').label, '等待返回', 'observing label');
})();

(function test_unknown_status() {
  const view = jobStatusView('paused');
  assertEqual(view.label, 'paused', 'unknown label keeps raw status');
  assertEqual(view.relayClass, 'badge-gray', 'unknown relay class');
})();

console.log('terminalJobStatus');

(function test_terminal_status() {
  assert(terminalJobStatus('succeeded'), 'succeeded is terminal');
  assert(terminalJobStatus('failed'), 'failed is terminal');
  assert(!terminalJobStatus('running'), 'running is not terminal');
})();

console.log('render badges');

(function test_relay_badge() {
  const html = renderJobBadge('timeout');
  assertContains(html, 'badge-red', 'timeout relay badge class');
  assertContains(html, '空闲超时', 'timeout relay badge label');
})();

(function test_bridge_badge() {
  const html = renderBridgeJobBadge('cancelled');
  assertContains(html, 'badge-deny', 'cancelled bridge badge type');
  assertContains(html, '已取消', 'cancelled bridge badge label');
})();

console.log('completion text');

(function test_completed_reason() {
  assertEqual(humanCompletedReason('stop_hook'), 'stop hook', 'stop hook bridge text');
  assertEqual(humanCompletedReason('stop_hook', { completedPrefix: true }), '已完成：stop hook', 'stop hook relay text');
  assertEqual(humanCompletedReason('process_exit_nonzero'), '进程异常', 'nonzero process text');
})();

(function test_completion_detail() {
  const detail = humanCompletionDetail({
    signal: '"StopHook"',
    authority: '"Daemon"',
    last_activity_at: 1700000000,
    hard_deadline_at: 1700000120,
  }, function (value) { return 'T' + value; });
  assertEqual(detail, 'StopHook · Daemon · activity T1700000000 · deadline T1700000120', 'completion detail');
})();

console.log('\n' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
