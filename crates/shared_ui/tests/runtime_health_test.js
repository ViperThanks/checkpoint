// runtime_health_test.js — 共享 runtime_health 模块的 fixture 驱动测试
//
// 职责：验证 runtime health UI 函数在各种 fixture 数据下的行为。
// 特别关注 critical 状态的可见性保证。

const path = require('path');
const { runtimeHealthBadge, runtimeAlertCard, runtimeHealthBanner, driftText, parseRuntimeHealth } = require('../runtime_health.js');

// ---- Test runner ----
let passed = 0;
let failed = 0;

function assert(condition, label) {
  if (condition) { passed++; }
  else { failed++; console.error('  FAIL: ' + label); }
}

function assertEqual(actual, expected, label) {
  const eq = JSON.stringify(actual) === JSON.stringify(expected);
  if (eq) { passed++; }
  else { failed++; console.error('  FAIL: ' + label); console.error('    expected: ' + JSON.stringify(expected)); console.error('    actual:   ' + JSON.stringify(actual)); }
}

function assertContains(str, substr, label) {
  if (str.indexOf(substr) >= 0) { passed++; }
  else { failed++; console.error('  FAIL: ' + label); console.error('    expected to contain: ' + substr); console.error('    actual: ' + str.substring(0, 200)); }
}

// ---- Load fixtures ----
const fs = require('fs');
const fixtureDir = path.join(__dirname, 'fixtures');

const overviewCritical = JSON.parse(fs.readFileSync(path.join(fixtureDir, 'overview_runtime_critical.json'), 'utf8'));
const convDetailCritical = JSON.parse(fs.readFileSync(path.join(fixtureDir, 'conversation_detail_runtime_critical.json'), 'utf8'));

// ---- runtimeHealthBadge ----

console.log('runtimeHealthBadge');

(function test_badge_critical() {
  assertContains(runtimeHealthBadge({ status: 'critical' }), 'badge-red', 'critical → badge-red');
  assertContains(runtimeHealthBadge({ status: 'critical' }), '环境漂移', 'critical → 环境漂移');
})();

(function test_badge_warning() {
  assertContains(runtimeHealthBadge({ status: 'warning' }), 'badge-yellow', 'warning → badge-yellow');
  assertContains(runtimeHealthBadge({ status: 'warning' }), '环境变更', 'warning → 环境变更');
})();

(function test_badge_ok_hidden() {
  assertEqual(runtimeHealthBadge({ status: 'ok' }), '', 'ok → hidden');
})();

(function test_badge_unknown_hidden() {
  assertEqual(runtimeHealthBadge({ status: 'unknown' }), '', 'unknown → hidden');
})();

(function test_badge_null_hidden() {
  assertEqual(runtimeHealthBadge(null), '', 'null → hidden');
  assertEqual(runtimeHealthBadge(undefined), '', 'undefined → hidden');
})();

// ---- runtimeAlertCard (fixture-driven) ----

console.log('runtimeAlertCard (fixture)');

(function test_fixture_overview_has_critical_alert() {
  const result = runtimeAlertCard(overviewCritical.conversations);
  assert(result.length > 0, 'fixture overview with critical → non-empty alert');
  assertContains(result, 'runtime-alert-card', 'contains alert card class');
  assertContains(result, '运行环境漂移', 'contains 运行环境漂移 text');
})();

(function test_fixture_critical_shows_first_title() {
  const result = runtimeAlertCard(overviewCritical.conversations);
  assertContains(result, 'Main development session', 'shows first critical conv title');
})();

(function test_fixture_multiple_critical_shows_count() {
  const result = runtimeAlertCard(overviewCritical.conversations);
  assertContains(result, '另有 1 个', 'shows count of additional critical convs');
})();

(function test_all_ok_no_alert() {
  const convos = [
    { id: '1', runtime_health: { status: 'ok' } },
    { id: '2', runtime_health: { status: 'ok' } },
  ];
  assertEqual(runtimeAlertCard(convos), '', 'all ok → no alert');
})();

(function test_empty_conversations_no_alert() {
  assertEqual(runtimeAlertCard([]), '', 'empty → no alert');
  assertEqual(runtimeAlertCard(null), '', 'null → no alert');
})();

// ---- runtimeHealthBanner (fixture-driven) ----

console.log('runtimeHealthBanner (fixture)');

(function test_fixture_critical_banner() {
  const result = runtimeHealthBanner(convDetailCritical);
  assert(result.length > 0, 'critical conv detail → non-empty banner');
  assertContains(result, 'runtime-critical', 'contains runtime-critical class');
  assertContains(result, '运行环境已漂移', 'contains drift warning text');
  assertContains(result, 'permissionMode', 'shows drifted field name');
  assertContains(result, 'Full Access', 'shows expected value');
  assertContains(result, 'Default', 'shows actual value');
})();

(function test_ok_conv_no_banner() {
  const conv = { id: '1', runtime_health: { status: 'ok' } };
  assertEqual(runtimeHealthBanner(conv), '', 'ok conv → no banner');
})();

(function test_warning_conv_has_banner() {
  const conv = {
    id: '1',
    runtime_health: {
      status: 'warning',
      warnings: [{ field: 'model', expected: 'sonnet', actual: 'haiku' }]
    }
  };
  const result = runtimeHealthBanner(conv);
  assertContains(result, 'runtime-warning', 'warning → runtime-warning class');
  assertContains(result, '运行环境有变更', 'warning → 有变更 text');
})();

// ---- driftText ----

console.log('driftText');

(function test_drift_text_with_warnings() {
  const health = {
    status: 'critical',
    warnings: [
      { field: 'permissionMode', expected: 'bypassPermissions', actual: 'default' }
    ]
  };
  const result = driftText(health);
  assertContains(result, 'permissionMode', 'drift text contains field');
  assertContains(result, 'Full Access', 'drift text contains expected');
  assertContains(result, 'Default', 'drift text contains actual');
})();

(function test_drift_text_no_warnings() {
  assertContains(driftText({ status: 'critical' }), '检测到运行环境不一致', 'no warnings → generic message');
})();

// ---- parseRuntimeHealth ----

console.log('parseRuntimeHealth');

(function test_parse_from_overview() {
  const conv = overviewCritical.conversations[0];
  const health = parseRuntimeHealth(conv);
  assert(health !== null, 'parseRuntimeHealth returns non-null');
  assertEqual(health.status, 'critical', 'status is critical');
})();

(function test_parse_null() {
  assertEqual(parseRuntimeHealth(null), null, 'null → null');
  assertEqual(parseRuntimeHealth(undefined), null, 'undefined → null');
})();

// ---- Summary ----

console.log('\n' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
