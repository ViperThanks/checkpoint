// app_test.js — 验证 shared_ui 的生产业务函数
//
// 本文件不复制任何业务函数实现，直接 require 生产模块。
// 如果 shared_ui 中的不变量被破坏，这些测试会失败。

const { buildNewJobBody, buildContinueJobBody } = require('../../../shared_ui/job_body.js');
const { shortId, escHtml, jsStr } = require('../../../shared_ui/view_model.js');
const { runtimeAlertCard, runtimeHealthBadge } = require('../../../shared_ui/runtime_health.js');
const { parseJwtExpMs, shouldRenewToken, shouldRunHeavyPoll } = require('./app.js');

// ---- Test runner ----

let passed = 0;
let failed = 0;

function assert(condition, label) {
  if (condition) {
    passed++;
  } else {
    failed++;
    console.error('  FAIL: ' + label);
  }
}

function assertEqual(actual, expected, label) {
  const eq = JSON.stringify(actual) === JSON.stringify(expected);
  if (eq) {
    passed++;
  } else {
    failed++;
    console.error('  FAIL: ' + label);
    console.error('    expected: ' + JSON.stringify(expected));
    console.error('    actual:   ' + JSON.stringify(actual));
  }
}

function assertThrows(fn, label) {
  try {
    fn();
    failed++;
    console.error('  FAIL: ' + label);
  } catch (_) {
    passed++;
  }
}

function fakeRelayToken(payload) {
  var body = Buffer.from(JSON.stringify(payload)).toString('base64url');
  return 'cp_rt1.' + body + '.sig';
}

// ---- buildNewJobBody ----

console.log('buildNewJobBody');

(function test_new_job_no_conversation_id() {
  var body = buildNewJobBody('claude_code', '/tmp/proj', 'hello');
  assert(!('conversation_id' in body), 'body must NOT contain conversation_id');
  assertEqual(body.kind, 'agent_prompt', 'kind is agent_prompt');
  assertEqual(body.provider, 'claude_code', 'provider is set');
  assertEqual(body.prompt, 'hello', 'prompt is set');
  assertEqual(body.project_path, '/tmp/proj', 'project_path is set');
})();

(function test_new_job_empty_project_excluded() {
  var body = buildNewJobBody('kimi_code', '', 'test');
  assert(!('conversation_id' in body), 'no conversation_id');
  assert(!('project_path' in body), 'empty project_path must not pollute body');
})();

(function test_new_job_null_project_excluded() {
  var body = buildNewJobBody('codex_cli', null, 'fix bug');
  assert(!('conversation_id' in body), 'no conversation_id');
  assert(!('project_path' in body), 'null project_path must not pollute body');
})();

(function test_new_job_codex() {
  var body = buildNewJobBody('codex_cli', '/tmp/proj', 'fix bug');
  assert(!('conversation_id' in body), 'codex new job has no conversation_id');
  assertEqual(body.provider, 'codex_cli', 'provider is codex_cli');
})();

// ---- buildContinueJobBody ----

console.log('buildContinueJobBody');

(function test_continue_has_conversation_id() {
  var body = buildContinueJobBody('claude_code', '/tmp/proj', 'sess-123', 'continue');
  assertEqual(body.conversation_id, 'sess-123', 'conversation_id is present');
  assertEqual(body.kind, 'agent_prompt', 'kind is agent_prompt');
  assertEqual(body.provider, 'claude_code', 'provider is set');
  assertEqual(body.prompt, 'continue', 'prompt is set');
})();

(function test_continue_codex_has_conversation_id() {
  var body = buildContinueJobBody('codex_cli', '/tmp/proj', 'tid-456', 'fix');
  assertEqual(body.conversation_id, 'tid-456', 'codex continue has conversation_id');
  assertEqual(body.provider, 'codex_cli', 'provider is codex_cli');
})();

(function test_continue_kimi_has_conversation_id() {
  var body = buildContinueJobBody('kimi_code', '/tmp/proj', 'sid-789', 'test');
  assertEqual(body.conversation_id, 'sid-789', 'kimi continue has conversation_id');
  assertEqual(body.provider, 'kimi_code', 'provider is kimi_code');
})();

(function test_continue_without_project() {
  var body = buildContinueJobBody('claude_code', null, 'sess-1', 'hello');
  assertEqual(body.project_path, undefined, 'project_path is undefined when null');
  assertEqual(body.conversation_id, 'sess-1', 'conversation_id still set');
})();

// 不允许静默降级：缺 conversation_id 必须 throw
(function test_continue_without_conversation_id_throws() {
  assertThrows(
    function () { buildContinueJobBody('codex_cli', '/tmp/proj', '', 'must not become new job'); },
    'empty conversation_id must throw'
  );
  assertThrows(
    function () { buildContinueJobBody('codex_cli', '/tmp/proj', null, 'must not become new job'); },
    'null conversation_id must throw'
  );
  assertThrows(
    function () { buildContinueJobBody('codex_cli', '/tmp/proj', undefined, 'must not become new job'); },
    'undefined conversation_id must throw'
  );
})();

// ---- view_model.js helpers ----

console.log('view_model helpers');

(function test_short_id_handles_empty_value() {
  assertEqual(shortId(''), '', 'empty id stays empty');
  assertEqual(shortId(null), '', 'null id stays empty');
})();

(function test_short_id_truncates_long_value() {
  assertEqual(shortId('019dd972-26ba'), '019dd972...', 'long id is shortened');
  assertEqual(shortId('short'), 'short', 'short id is unchanged');
})();

(function test_esc_html_escapes_special_chars() {
  assertEqual(escHtml('<script>alert("xss")</script>'), '&lt;script&gt;alert(&quot;xss&quot;)&lt;/script&gt;', 'escHtml escapes XSS');
  assertEqual(escHtml('a&b'), 'a&amp;b', 'escHtml escapes ampersand');
  assertEqual(escHtml(''), '', 'escHtml handles empty');
  assertEqual(escHtml(null), '', 'escHtml handles null');
})();

(function test_js_str_escapes_quotes() {
  assertEqual(jsStr("it's"), "it\\'s", 'jsStr escapes single quotes');
  assertEqual(jsStr('say "hi"'), 'say \\"hi\\"', 'jsStr escapes double quotes');
  assertEqual(jsStr('a\\b'), 'a\\\\b', 'jsStr escapes backslash');
  assertEqual(jsStr(''), '', 'jsStr handles empty');
})();

// ---- Runtime Health UI rendering ----

console.log('runtime health rendering');

(function test_alert_card_no_critical() {
  var convos = [
    { id: '1', title: 'ok conv', runtime_health: { status: 'ok' } }
  ];
  var result = runtimeAlertCard(convos);
  assertEqual(result, '', 'no critical → no alert card');
})();

(function test_alert_card_with_critical() {
  var convos = [
    { id: 'abc123', title: 'Drifted Session', runtime_health: { status: 'critical' } },
    { id: 'def456', title: 'Ok Session', runtime_health: { status: 'ok' } }
  ];
  var result = runtimeAlertCard(convos);
  assert(result.indexOf('runtime-alert-card') >= 0, 'critical → HTML contains runtime-alert-card');
  assert(result.indexOf('运行环境漂移') >= 0, 'alert text contains 运行环境漂移');
  assert(result.indexOf('Drifted Session') >= 0, 'alert shows conversation title');
  assert(result.indexOf('abc123') >= 0, 'alert links to critical conversation id');
})();

(function test_alert_card_multiple_critical() {
  var convos = [
    { id: 'a1', title: 'First', runtime_health: { status: 'critical' } },
    { id: 'a2', title: 'Second', runtime_health: { status: 'critical' } },
    { id: 'a3', title: 'Third', runtime_health: { status: 'critical' } }
  ];
  var result = runtimeAlertCard(convos);
  assert(result.indexOf('另有 2 个') >= 0, 'multiple critical → shows count');
})();

(function test_badge_critical() {
  var result = runtimeHealthBadge({ status: 'critical' });
  assert(result.indexOf('badge-red') >= 0, 'critical badge has badge-red class');
  assert(result.indexOf('环境漂移') >= 0, 'critical badge text');
})();

(function test_badge_warning() {
  var result = runtimeHealthBadge({ status: 'warning' });
  assert(result.indexOf('badge-yellow') >= 0, 'warning badge has badge-yellow class');
  assert(result.indexOf('环境变更') >= 0, 'warning badge text');
})();

(function test_badge_unknown_returns_empty() {
  var result = runtimeHealthBadge({ status: 'unknown' });
  assertEqual(result, '', 'unknown → no badge');
})();

(function test_badge_null_returns_empty() {
  assertEqual(runtimeHealthBadge(null), '', 'null health → no badge');
  assertEqual(runtimeHealthBadge(undefined), '', 'undefined health → no badge');
})();

(function test_badge_ok_returns_empty() {
  assertEqual(runtimeHealthBadge({ status: 'ok' }), '', 'ok → no badge');
})();

// ---- Mobile session lifecycle helpers ----

console.log('mobile session lifecycle helpers');

(function test_parse_jwt_exp_ms() {
  var token = fakeRelayToken({ ver: 1, sid: 'sid', role: 'client', exp: 2000 });
  assertEqual(parseJwtExpMs(token), 2000000, 'parseJwtExpMs reads exp seconds as ms');
})();

(function test_parse_jwt_exp_ms_bad_token() {
  assertEqual(parseJwtExpMs('bad-token'), null, 'parseJwtExpMs returns null for invalid token');
})();

(function test_should_renew_token() {
  var now = 100000;
  assert(shouldRenewToken(now + 1000, now, 5000), 'renew when inside skew');
  assert(!shouldRenewToken(now + 10000, now, 5000), 'do not renew when outside skew');
  assert(!shouldRenewToken(null, now, 5000), 'do not renew without exp');
})();

(function test_hidden_disables_heavy_poll() {
  assert(!shouldRunHeavyPoll('hidden', 'online'), 'hidden page must not run heavy poll');
  assert(!shouldRunHeavyPoll('visible', 'offline'), 'offline state must not run heavy poll');
  assert(shouldRunHeavyPoll('visible', 'online'), 'visible online page may poll');
})();

// ---- Summary ----

console.log('\n' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
