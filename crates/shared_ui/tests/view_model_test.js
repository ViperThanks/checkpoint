// view_model_test.js — 共享 view_model 模块测试
//
// 验证 escHtml, jsStr, shortId, trunc, formatTime, relTime, agentLabel,
// permissionModeLabel, cleanAgentLogChunk。

const {
  escHtml, jsStr, shortId, trunc, formatTime, relTime, agentLabel, permissionModeLabel,
  shortProject, projectBasename, cleanAgentLogChunk,
} = require('../view_model.js');

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

// ---- escHtml ----

console.log('escHtml');

(function test_escapes_html_entities() {
  assertEqual(escHtml('<script>alert("xss")</script>'), '&lt;script&gt;alert(&quot;xss&quot;)&lt;/script&gt;', 'full XSS');
  assertEqual(escHtml('a&b'), 'a&amp;b', 'ampersand');
  assertEqual(escHtml('a<b'), 'a&lt;b', 'less than');
  assertEqual(escHtml('a>b'), 'a&gt;b', 'greater than');
  assertEqual(escHtml('a"b'), 'a&quot;b', 'double quote');
})();

(function test_handles_edge_cases() {
  assertEqual(escHtml(''), '', 'empty string');
  assertEqual(escHtml(null), '', 'null');
  assertEqual(escHtml(undefined), '', 'undefined');
  assertEqual(escHtml(0), '0', 'number zero → "0"');
  assertEqual(escHtml(false), 'false', 'boolean false → "false"');
})();

// ---- jsStr ----

console.log('jsStr');

(function test_escapes_js_specials() {
  assertEqual(jsStr("it's"), "it\\'s", 'single quote');
  assertEqual(jsStr('say "hi"'), 'say \\"hi\\"', 'double quote');
  assertEqual(jsStr('a\\b'), 'a\\\\b', 'backslash');
  assertEqual(jsStr('line1\nline2'), 'line1\\nline2', 'newline');
  assertEqual(jsStr('line1\rline2'), 'line1\\rline2', 'carriage return');
})();

(function test_handles_edge_cases() {
  assertEqual(jsStr(''), '', 'empty');
  assertEqual(jsStr(null), '', 'null');
  assertEqual(jsStr(undefined), '', 'undefined');
  assertEqual(jsStr(0), '0', 'number zero → "0"');
})();

// ---- shortId ----

console.log('shortId');

(function test_short_id() {
  assertEqual(shortId(''), '', 'empty');
  assertEqual(shortId(null), '', 'null');
  assertEqual(shortId('short'), 'short', 'short stays');
  assertEqual(shortId('019dd972-26ba'), '019dd972...', 'long truncated');
  assertEqual(shortId('12345678'), '12345678', 'exactly 8 chars');
  assertEqual(shortId('123456789'), '12345678...', '9 chars truncated');
})();

// ---- trunc ----

console.log('trunc');

(function test_trunc() {
  assertEqual(trunc('', 5), '', 'empty');
  assertEqual(trunc(null, 5), '', 'null');
  assertEqual(trunc('hello', 10), 'hello', 'short enough');
  assertEqual(trunc('hello world', 5), 'hello...', 'truncated');
  assertEqual(trunc('abc', 3), 'abc', 'exactly n');
})();

// ---- agentLabel ----

console.log('agentLabel');

(function test_agent_labels() {
  assertEqual(agentLabel('claude_code'), 'Claude Code', 'claude_code');
  assertEqual(agentLabel('codex_cli'), 'Codex CLI', 'codex_cli');
  assertEqual(agentLabel('kimi_code'), 'Kimi Code', 'kimi_code');
  assertEqual(agentLabel('gemini_cli'), 'Gemini CLI', 'gemini_cli');
  assertEqual(agentLabel('claude'), 'Claude Code', 'short alias claude');
  assertEqual(agentLabel('codex'), 'Codex CLI', 'short alias codex');
  assertEqual(agentLabel('unknown_agent'), 'unknown_agent', 'unknown passthrough');
  assertEqual(agentLabel(''), '未知', 'empty → 未知');
  assertEqual(agentLabel(null), '未知', 'null → 未知');
})();

// ---- permissionModeLabel ----

console.log('permissionModeLabel');

(function test_permission_mode_labels() {
  assertEqual(permissionModeLabel('bypassPermissions'), 'Full Access', 'bypassPermissions');
  assertEqual(permissionModeLabel('danger-full-access'), 'Full Access', 'danger full access');
  assertEqual(permissionModeLabel('workspace_write'), 'Workspace Write', 'workspace_write');
  assertEqual(permissionModeLabel('read-only'), 'Read Only', 'read only');
  assertEqual(permissionModeLabel('default'), 'Default', 'default');
  assertEqual(permissionModeLabel('unknown'), 'unknown', 'unknown passthrough');
  assertEqual(permissionModeLabel(''), '', 'empty');
})();

// ---- shortProject ----

console.log('shortProject');

(function test_short_project() {
  assertEqual(shortProject(''), '', 'empty');
  assertEqual(shortProject('/Users/dev/myproject'), 'dev/myproject', 'last 2 parts');
  assertEqual(shortProject('/a'), '/a', 'single part');
})();

// ---- projectBasename ----

console.log('projectBasename');

(function test_project_basename() {
  assertEqual(projectBasename(''), '', 'empty');
  assertEqual(projectBasename('/Users/dev/myproject'), 'myproject', 'basename');
  assertEqual(projectBasename('/a/b/'), 'b', 'trailing slash');
})();

// ---- cleanAgentLogChunk ----

console.log('cleanAgentLogChunk');

(function test_filters_internal_noise() {
  const log = { stream: 'stdout', chunk: 'TurnBegin(claude)\nHello world\nStepEnd()\n' };
  const result = cleanAgentLogChunk(log);
  assertEqual(result, 'Hello world', 'filters TurnBegin/StepEnd');
})();

(function test_filters_stderr_non_error() {
  const log = { stream: 'stderr', chunk: 'some warning\nan error occurred\nanother info line\n' };
  const result = cleanAgentLogChunk(log);
  assertEqual(result, 'an error occurred', 'stderr keeps only error lines');
})();

(function test_filters_codex_nonfatal_auth_noise() {
  const log = {
    stream: 'stderr',
    chunk: '2026-05-01T05:03:31.335410Z ERROR rmcp::transport::worker: worker quit with fatal: Transport channel closed, when Auth(TokenRefreshFailed("Server returned error response: invalid_grant: Invalid refresh token"))'
  };
  assertEqual(cleanAgentLogChunk(log), '', 'filters nonfatal Codex token refresh noise');
})();

(function test_filters_codex_rollout_record_noise() {
  const log = {
    stream: 'stderr',
    chunk: '2026-04-30T16:35:09.258041Z ERROR codex_core::session: failed to record rollout items: thread 019ddf3e-61d6-7851-931a-17f3f96fa642 not found'
  };
  assertEqual(cleanAgentLogChunk(log), '', 'filters nonfatal Codex rollout record noise');
})();

(function test_empty_input() {
  assertEqual(cleanAgentLogChunk(null), '', 'null');
  assertEqual(cleanAgentLogChunk({}), '', 'no stream');
  assertEqual(cleanAgentLogChunk({ stream: 'stdout', chunk: '' }), '', 'empty chunk');
})();

(function test_preserves_meaningful_stdout() {
  const log = { stream: 'stdout', chunk: 'Building...\nCompiling 5 files\nDone\n' };
  const result = cleanAgentLogChunk(log);
  assert(result.indexOf('Building...') >= 0, 'preserves meaningful lines');
})();

// ---- formatTime ----

console.log('formatTime');

(function test_format_time() {
  assertEqual(formatTime(''), '', 'empty');
  assertEqual(formatTime(null), '', 'null');
  // Valid ISO string should return non-empty
  const result = formatTime('2026-05-01T10:30:00Z');
  assert(result.length > 0, 'valid ISO → non-empty');
})();

// ---- relTime ----

console.log('relTime');

(function test_rel_time() {
  assertEqual(relTime(''), '', 'empty');
  assertEqual(relTime(null), '', 'null');
  // Recent time should return 刚刚
  const recent = new Date(Date.now() - 5000).toISOString();
  assertEqual(relTime(recent), '刚刚', '5 seconds ago → 刚刚');
})();

// ---- Summary ----

console.log('\n' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
