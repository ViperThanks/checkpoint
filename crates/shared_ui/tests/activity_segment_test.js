// activity_segment_test.js — Activity Segment 回归测试
//
// 覆盖：工具分类、段构建、Turn 分组、摘要渲染、运行中态、折叠。

var path = require('path');
var as = require(path.join(__dirname, '..', 'activity_segment.js'));

var passed = 0, failed = 0;

function assert(cond, msg) {
  if (cond) { passed++; }
  else { failed++; console.log('FAIL: ' + msg); }
}

function assertEq(a, b, msg) {
  if (JSON.stringify(a) === JSON.stringify(b)) { passed++; }
  else { failed++; console.log('FAIL: ' + msg + '\n  expected: ' + JSON.stringify(b) + '\n  actual:   ' + JSON.stringify(a)); }
}

// ============================================================
// classifyTool
// ============================================================

console.log('classifyTool');
assert(as.classifyTool('Read').category === 'explore', 'Read → explore');
assert(as.classifyTool('Read').subcategory === 'file', 'Read → file');
assert(as.classifyTool('Grep').category === 'explore', 'Grep → explore');
assert(as.classifyTool('Grep').subcategory === 'search', 'Grep → search');
assert(as.classifyTool('Bash').category === 'explore', 'Bash → explore');
assert(as.classifyTool('Bash').subcategory === 'command', 'Bash → command');
assert(as.classifyTool('exec_command').subcategory === 'command', 'Codex exec_command → command');
assert(as.classifyTool('write_stdin').subcategory === 'command', 'Codex write_stdin → command');
assert(as.classifyTool('Edit').category === 'edit', 'Edit → edit');
assert(as.classifyTool('Write').category === 'edit', 'Write → edit');
assert(as.classifyTool('WriteFile').category === 'edit', 'WriteFile → edit');
assert(as.classifyTool('StrReplaceFile').category === 'edit', 'StrReplaceFile → edit');
assert(as.classifyTool('Glob').category === 'explore', 'Glob → explore');
assert(as.classifyTool('WebSearch').category === 'explore', 'WebSearch → explore');
assert(as.classifyTool(null) === null, 'null → null');
assert(as.classifyTool('UnknownTool') === null, 'Unknown → null');
assert(as.classifyTool('TodoRead') === null, 'TodoRead → null');
assert(as.classifyTool('TodoWrite') === null, 'TodoWrite → null');

// Gemini tools
assert(as.classifyTool('read_file').category === 'explore', 'read_file → explore');
assert(as.classifyTool('read_file').subcategory === 'file', 'read_file → file');
assert(as.classifyTool('write_file').category === 'edit', 'write_file → edit');
assert(as.classifyTool('run_shell_command').category === 'explore', 'run_shell_command → explore');
assert(as.classifyTool('run_shell_command').subcategory === 'command', 'run_shell_command → command');
// Codex tools
assert(as.classifyTool('ReadFile').category === 'explore', 'ReadFile → explore');
assert(as.classifyTool('web_search').category === 'explore', 'web_search → explore');
assert(as.classifyTool('web_search').subcategory === 'search', 'web_search → search');

// ============================================================
// formatDuration
// ============================================================

console.log('formatDuration');
assertEq(as.formatDuration(0), '');
assertEq(as.formatDuration(500), '< 1s');
assertEq(as.formatDuration(1000), '1s');
assertEq(as.formatDuration(21000), '21s');
assertEq(as.formatDuration(60000), '1m 0s');
assertEq(as.formatDuration(201000), '3m 21s');
assertEq(as.formatDuration(3720000), '1h 2m');
assertEq(as.formatDuration(-100), '');
assertEq(as.formatDuration(null), '');
assertEq(as.formatDuration(undefined), '');

// ============================================================
// buildSegments — 空输入
// ============================================================

console.log('buildSegments empty');
assertEq(as.buildSegments(null), []);
assertEq(as.buildSegments([]), []);
assertEq(as.buildSegments(undefined), []);

// ============================================================
// buildSegments — 纯聊天
// ============================================================

console.log('buildSegments chat only');
var chatOnly = [
  { role: 'user', text: 'hi', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'assistant', text: 'hello', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
];
var segs = as.buildSegments(chatOnly);
assertEq(segs.length, 2);
assertEq(segs[0].type, 'user');
assertEq(segs[0].message.text, 'hi');
assertEq(segs[1].type, 'assistant');
assertEq(segs[1].message.text, 'hello');

// ============================================================
// buildSegments — 连续同类工具合并
// ============================================================

console.log('buildSegments merge explore');
var exploreMerge = [
  { role: 'user', text: 'look at the bug', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'src/main.rs', timestamp: '2026-05-01T10:00:02Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Grep', tool_input_preview: '"TODO"', timestamp: '2026-05-01T10:00:03Z', seq: 3 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'src/lib.rs', timestamp: '2026-05-01T10:00:04Z', seq: 4 },
  { role: 'assistant', text: 'found it', timestamp: '2026-05-01T10:00:05Z', seq: 5 },
];
segs = as.buildSegments(exploreMerge);
// user, explore(3 items), assistant
assertEq(segs.length, 3);
assertEq(segs[0].type, 'user');
assertEq(segs[1].type, 'explore');
assertEq(segs[1].items.length, 3);
assertEq(segs[1].fileCount, 2, '2 file reads');
assertEq(segs[1].searchCount, 1, '1 grep search');
assertEq(segs[1].commandCount, 0);
assert(segs[1].summary.indexOf('main.rs') >= 0 || segs[1].summary.indexOf('lib.rs') >= 0, 'summary has file name');
assert(segs[1].summary.indexOf('TODO') >= 0, 'summary has search term');
assertEq(segs[2].type, 'assistant');

// ============================================================
// buildSegments — explore + edit 分类切换
// ============================================================

console.log('buildSegments category switch');
var mixedSegs = [
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'cargo test', timestamp: '2026-05-01T10:00:02Z', seq: 3 },
];
segs = as.buildSegments(mixedSegs);
// explore(Read), edit(Edit), explore(Bash)
assertEq(segs.length, 3);
assertEq(segs[0].type, 'explore');
assertEq(segs[0].items.length, 1);
assertEq(segs[1].type, 'edit');
assertEq(segs[1].items.length, 1);
assertEq(segs[2].type, 'explore');
assertEq(segs[2].items.length, 1);

// ============================================================
// buildSegments — 含 Bash 的 explore 段
// ============================================================

console.log('buildSegments explore with command');
var cmdExplore = [
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'f.rs', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'cargo build', timestamp: '2026-05-01T10:00:05Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Grep', tool_input_preview: '"error"', timestamp: '2026-05-01T10:00:06Z', seq: 3 },
];
segs = as.buildSegments(cmdExplore);
// All explore, merged into one segment
assertEq(segs.length, 1);
assertEq(segs[0].type, 'explore');
assertEq(segs[0].items.length, 3);
assertEq(segs[0].fileCount, 1, '1 file read');
assertEq(segs[0].commandCount, 1, '1 bash command');
assertEq(segs[0].searchCount, 1, '1 grep');
assert(segs[0].summary.indexOf('f.rs') >= 0 || segs[0].summary.indexOf('cargo build') >= 0, 'summary: has real name');
// 3 items → joined details
assertEq(segs[0].duration, 6000, '6s duration');

// ============================================================
// buildSegments — edit 段
// ============================================================

console.log('buildSegments edit');
var editSegs = [
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'a.rs old→new', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Write', tool_input_preview: 'b.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
];
segs = as.buildSegments(editSegs);
assertEq(segs.length, 1);
assertEq(segs[0].type, 'edit');
assertEq(segs[0].items.length, 2);
assertEq(segs[0].editCount, 2);
assert(segs[0].summary.indexOf('2') >= 0 && segs[0].summary.indexOf('文件') >= 0, 'edit summary: 修改 2 个文件');

// ============================================================
// buildSegments — 未知工具单独通过
// ============================================================

console.log('buildSegments unknown tool passthrough');
var unknownTool = [
  { role: 'tool_summary', tool_name: 'Agent', tool_input_preview: 'spawn', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
];
segs = as.buildSegments(unknownTool);
assertEq(segs.length, 1);
assertEq(segs[0].type, 'tool');
assertEq(segs[0].summary, 'Agent');

// ============================================================
// buildSegments — 完整场景
// ============================================================

console.log('buildSegments full scenario');
var fullScenario = [
  { role: 'user', text: 'fix the bug', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'assistant', text: 'looking...', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'main.rs', timestamp: '2026-05-01T10:00:02Z', seq: 3 },
  { role: 'tool_summary', tool_name: 'Grep', tool_input_preview: '"TODO"', timestamp: '2026-05-01T10:00:03Z', seq: 4 },
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'cargo test', timestamp: '2026-05-01T10:00:10Z', seq: 5 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'main.rs fix', timestamp: '2026-05-01T10:00:15Z', seq: 6 },
  { role: 'assistant', text: 'done', timestamp: '2026-05-01T10:00:16Z', seq: 7 },
];
segs = as.buildSegments(fullScenario);
// user, assistant, explore(3), edit(1), assistant
assertEq(segs.length, 5);
assertEq(segs[0].type, 'user');
assertEq(segs[1].type, 'assistant');
assertEq(segs[2].type, 'explore');
assertEq(segs[2].items.length, 3);
assert(segs[2].summary.length > 0, 'full: explore summary not empty');
assert(segs[2].summary.indexOf('main.rs') >= 0 || segs[2].summary.length > 0, 'full: has meaningful content');
assertEq(segs[3].type, 'edit');
assertEq(segs[3].items.length, 1);
assertEq(segs[4].type, 'assistant');

// ============================================================
// buildSegments — duration 计算
// ============================================================

console.log('buildSegments duration');
var durSegs = [
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'a', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'b', timestamp: '2026-05-01T10:00:05Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'c', timestamp: '2026-05-01T10:00:12Z', seq: 3 },
];
segs = as.buildSegments(durSegs);
assertEq(segs.length, 1);
assertEq(segs[0].duration, 12000, '12s between first and last');

// ============================================================
// buildSegments — 无 timestamp
// ============================================================

console.log('buildSegments no timestamp');
var noTs = [
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'a', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'b', seq: 2 },
];
segs = as.buildSegments(noTs);
assertEq(segs.length, 1);
assertEq(segs[0].duration, 0, 'no timestamp → 0 duration');

// ============================================================
// buildTurnGroups — 基本分组
// ============================================================

console.log('buildTurnGroups basic');
var turnSegs = as.buildSegments([
  { role: 'user', text: 'q1', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'assistant', text: 'a1', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'f', timestamp: '2026-05-01T10:00:02Z', seq: 3 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'f', timestamp: '2026-05-01T10:00:05Z', seq: 4 },
  { role: 'assistant', text: 'done', timestamp: '2026-05-01T10:00:06Z', seq: 5 },
  { role: 'user', text: 'q2', timestamp: '2026-05-01T10:01:00Z', seq: 6 },
  { role: 'assistant', text: 'a2', timestamp: '2026-05-01T10:01:01Z', seq: 7 },
]);
var groups = as.buildTurnGroups(turnSegs);
// 2 groups: q1 → tools → done, q2 → a2
assertEq(groups.length, 2);
assertEq(groups[0].userSeg.type, 'user');
assertEq(groups[0].segments.length, 4, 'assistant + explore + edit + assistant');
assertEq(groups[0].toolCount, 2, '2 tool invocations');
assert(!groups[0].isRunning, 'first group not running');
assertEq(groups[1].segments.length, 1, 'just assistant');
assertEq(groups[1].toolCount, 0, 'no tools in second group');

// ============================================================
// buildTurnGroups — isLastRunning
// ============================================================

console.log('buildTurnGroups running');
var runSegs = as.buildSegments([
  { role: 'user', text: 'q', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'f', timestamp: '2026-05-01T10:00:02Z', seq: 2 },
]);
groups = as.buildTurnGroups(runSegs, true);
assertEq(groups.length, 1);
assert(groups[0].isRunning, 'last group is running');

groups = as.buildTurnGroups(runSegs, false);
assert(!groups[0].isRunning, 'not running when flag false');

// ============================================================
// buildTurnGroups — 无用户消息开头
// ============================================================

console.log('buildTurnGroups no leading user');
var noUser = as.buildSegments([
  { role: 'assistant', text: 'orphan', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'f', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
]);
groups = as.buildTurnGroups(noUser);
assertEq(groups.length, 1);
assertEq(groups[0].userSeg, null, 'no user segment');
assertEq(groups[0].segments.length, 2);

// ============================================================
// renderSegmentCard — HTML 输出
// ============================================================

console.log('renderSegmentCard');
var cardSeg = {
  type: 'explore',
  items: [
    { tool_name: 'Read', tool_input_preview: 'a.rs' },
    { tool_name: 'Grep', tool_input_preview: '"TODO"' },
  ],
  summary: 'a.rs · TODO',
  startTime: null, endTime: null, duration: 0,
  fileCount: 2, searchCount: 1, commandCount: 0, editCount: 0,
};
var html = as.renderSegmentCard(cardSeg, 0);
assert(html.indexOf('act-card') >= 0, 'has act-card class');
assert(html.indexOf('act-explore') >= 0, 'has act-explore class');
assert(html.indexOf('a.rs') >= 0, 'summary text in HTML');
assert(html.indexOf('toggleActDetail') >= 0, 'has toggle for 2+ items');
assert(html.indexOf('toggleActDetail(this)') >= 0, 'segment toggle is element-local');
assert(html.indexOf('act-detail') >= 0, 'has detail div');

// Single item — no toggle
var singleSeg = {
  type: 'edit',
  items: [{ tool_name: 'Edit', tool_input_preview: 'a.rs' }],
  summary: '编辑 a.rs',
  startTime: null, endTime: null, duration: 0,
  fileCount: 0, searchCount: 0, commandCount: 0, editCount: 1,
};
html = as.renderSegmentCard(singleSeg, 1);
assert(html.indexOf('act-edit') >= 0, 'single: has act-edit');
assert(html.indexOf('toggleActDetail') >= 0, 'single: has toggle');
assert(html.indexOf('act-detail-inline') < 0, 'single: no inline detail');
assert(html.indexOf('a.rs') >= 0, 'single: preview is visible');

// ============================================================
// renderSegmentDetail — 展开详情
// ============================================================

console.log('renderSegmentDetail');
var detail = as.renderSegmentDetail(cardSeg);
assert(detail.indexOf('读取文件') >= 0, 'detail: has 读取文件');
assert(detail.indexOf('搜索内容') >= 0, 'detail: has 搜索内容');
assert(detail.indexOf('a.rs') >= 0, 'detail: has preview');
assert(detail.indexOf('act-item') >= 0, 'detail: has act-item class');

// ============================================================
// renderTurnBanner — 完成态
// ============================================================

console.log('renderTurnBanner settled');
var settledGroup = {
  segments: [cardSeg],
  startTime: '2026-05-01T10:00:00Z',
  endTime: '2026-05-01T10:03:21Z',
  duration: 201000,
  isRunning: false,
  toolCount: 2,
};
html = as.renderTurnBanner(settledGroup, 0);
assert(html.indexOf('act-settled') >= 0, 'settled: has act-settled');
assert(html.indexOf('耗时') >= 0, 'settled: has duration label');
assert(html.indexOf('3m 21s') >= 0, 'settled: has duration');
assert(html.indexOf('2 次操作') >= 0, 'settled: has action count');
assert(html.indexOf('toggleTurnBody') >= 0, 'settled: has toggle');
assert(html.indexOf('toggleTurnBody(this)') >= 0, 'turn toggle is element-local');

// ============================================================
// renderTurnBanner — 运行中态
// ============================================================

console.log('renderTurnBanner running');
var runningGroup = {
  segments: [cardSeg],
  startTime: '2026-05-01T10:00:00Z',
  endTime: '2026-05-01T10:00:05Z',
  duration: 5000,
  isRunning: true,
  toolCount: 2,
};
html = as.renderTurnBanner(runningGroup, 1);
assert(html.indexOf('act-running') >= 0, 'running: has act-running');
assert(html.indexOf('act-pulse') >= 0, 'running: has pulse indicator');
assert(html.indexOf('处理中') >= 0, 'running: has running label');
assert(html.indexOf('toggleTurnBody') < 0, 'running: no toggle on banner');

// ============================================================
// renderTurnBanner — 无工具活动
// ============================================================

console.log('renderTurnBanner no tools');
var emptyGroup = {
  segments: [],
  startTime: null, endTime: null, duration: 0,
  isRunning: false, toolCount: 0,
};
html = as.renderTurnBanner(emptyGroup, 2);
assertEq(html, '', 'empty group → empty string');

// ============================================================
// Kimi 工具名映射
// ============================================================

console.log('Kimi tools');
assert(as.classifyTool('WriteFile').category === 'edit', 'WriteFile → edit');
assert(as.classifyTool('StrReplaceFile').category === 'edit', 'StrReplaceFile → edit');
var kimiSegs = as.buildSegments([
  { role: 'tool_summary', tool_name: 'StrReplaceFile', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'WriteFile', tool_input_preview: 'b.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
]);
assertEq(kimiSegs.length, 1);
assertEq(kimiSegs[0].type, 'edit');
assertEq(kimiSegs[0].items.length, 2);

// ============================================================
// Codex 命令工具压缩
// ============================================================

console.log('Codex command tools');
var codexCmdSegs = as.buildSegments([
  { role: 'tool_summary', tool_name: 'exec_command', tool_input_preview: 'git status', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'write_stdin', tool_input_preview: '{"session_id":1}', timestamp: '2026-05-01T10:00:02Z', seq: 2 },
]);
assertEq(codexCmdSegs.length, 1, 'Codex command tools merge');
assertEq(codexCmdSegs[0].commandCount, 2, 'Codex command count');
assert(codexCmdSegs[0].summary.indexOf('git status') >= 0 || codexCmdSegs[0].summary.length > 0, 'Codex summary not empty');

// ============================================================
// 长序列合并
// ============================================================

console.log('long sequence merge');
var longSeq = [];
for (var i = 0; i < 20; i++) {
  longSeq.push({ role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'file' + i + '.rs', timestamp: '2026-05-01T10:00:' + (i < 10 ? '0' + i : i) + 'Z', seq: i + 1 });
}
segs = as.buildSegments(longSeq);
assertEq(segs.length, 1, '20 Reads → 1 segment');
assertEq(segs[0].items.length, 20);
assertEq(segs[0].fileCount, 20);

// ============================================================
// 交替 explore/edit 序列
// ============================================================

console.log('alternating categories');
var alternating = [
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'a', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'a', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'b', timestamp: '2026-05-01T10:00:02Z', seq: 3 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'b', timestamp: '2026-05-01T10:00:03Z', seq: 4 },
];
segs = as.buildSegments(alternating);
assertEq(segs.length, 4, 'alternating → 4 segments');
assertEq(segs[0].type, 'explore');
assertEq(segs[1].type, 'edit');
assertEq(segs[2].type, 'explore');
assertEq(segs[3].type, 'edit');

// ============================================================
// extractFileName
// ============================================================

console.log('extractFileName');
assertEq(as.extractFileName('src/main.rs'), 'main.rs');
assertEq(as.extractFileName('/Users/x/proj/lib.rs'), 'lib.rs');
assertEq(as.extractFileName('Cargo.toml'), 'Cargo.toml');
assertEq(as.extractFileName(''), '');
assertEq(as.extractFileName(null), '');
assertEq(as.extractFileName(undefined), '');

// ============================================================
// extractCommand
// ============================================================

console.log('extractCommand');
var cmd = as.extractCommand({ tool_name: 'Bash', tool_input_preview: 'cargo test' });
assertEq(cmd.label, '运行命令');
assertEq(cmd.detail, 'cargo test');

cmd = as.extractCommand({ tool_name: 'Read', tool_input_preview: 'src/main.rs' });
assertEq(cmd.label, '读取文件');
assertEq(cmd.detail, 'main.rs');

cmd = as.extractCommand({ tool_name: 'Edit', tool_input_preview: '/Users/x/proj/bridge.rs' });
assertEq(cmd.label, '编辑文件');
assertEq(cmd.detail, 'bridge.rs');

cmd = as.extractCommand({ tool_name: 'Unknown', tool_input_preview: 'whatever' });
assertEq(cmd.label, 'Unknown');
assertEq(cmd.detail, 'whatever');

// ============================================================
// maskSensitive
// ============================================================

console.log('maskSensitive');
var masked = as.maskSensitive('password=abc123token');
assert(masked.indexOf('abc123token') === -1, 'mask password value');
assert(masked.indexOf('password=') >= 0, 'keep password key');
assert(masked.indexOf('****') >= 0, 'has mask marker');

masked = as.maskSensitive('export TOKEN=deadbeef0123456789abcdef0123456789abcdef01234567');
assert(masked.indexOf('deadbeef') === -1, 'mask long hex token');
assert(masked.indexOf('****') >= 0, 'long hex replaced');

assertEq(as.maskSensitive('cargo test'), 'cargo test', 'no mask for safe text');
assertEq(as.maskSensitive(''), '', 'empty string');
assertEq(as.maskSensitive(null), null, 'null passthrough');
assertEq(as.maskSensitive(undefined), undefined, 'undefined passthrough');

masked = as.maskSensitive('secret=mysecret123');
assert(masked.indexOf('mysecret123') === -1, 'mask secret value');

// ============================================================
// buildSegments — 语义化摘要
// ============================================================

console.log('buildSegments semantic summary');

// 单个命令 → "运行命令 cargo test"
var singleCmd = [
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'cargo test', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
];
segs = as.buildSegments(singleCmd);
assertEq(segs[0].summary, '运行命令 cargo test', 'single command summary');

// 单个文件读取 → "读取文件 main.rs"
var singleFile = [
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'src/main.rs', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
];
segs = as.buildSegments(singleFile);
assertEq(segs[0].summary, '读取文件 main.rs', 'single file summary');

// 单个编辑 → "编辑 bridge.rs"
var singleEdit = [
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'src/bridge.rs', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
];
segs = as.buildSegments(singleEdit);
assertEq(segs[0].summary, '编辑 bridge.rs', 'single edit summary');

// 多操作 (>3) → 分类计数
var manyOps = [];
for (var k = 0; k < 5; k++) {
  manyOps.push({ role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'file' + k + '.rs', timestamp: '2026-05-01T10:00:0' + k + 'Z', seq: k + 1 });
}
segs = as.buildSegments(manyOps);
assert(segs[0].summary.indexOf('5') >= 0, 'many ops: count shown');
assert(segs[0].summary.indexOf('文件') >= 0, 'many ops: has 文件 label');

// ============================================================
// renderSegmentDetail — 中文工具名 + 脱敏
// ============================================================

console.log('renderSegmentDetail Chinese + masking');
var sensitiveSeg = {
  type: 'explore',
  items: [
    { tool_name: 'Bash', tool_input_preview: 'echo password=supersecret123' },
    { tool_name: 'Read', tool_input_preview: 'src/config.rs' },
  ],
};
var detailHtml = as.renderSegmentDetail(sensitiveSeg);
assert(detailHtml.indexOf('运行命令') >= 0, 'detail: Chinese name for Bash');
assert(detailHtml.indexOf('读取文件') >= 0, 'detail: Chinese name for Read');
assert(detailHtml.indexOf('supersecret123') === -1, 'detail: password masked');
assert(detailHtml.indexOf('config.rs') >= 0, 'detail: file name visible');
assert(detailHtml.indexOf('act-item') >= 0, 'detail: has act-item class');

// ============================================================
// renderTurnBanner — 中文标签
// ============================================================

console.log('renderTurnBanner compact labels');
var cnGroup = {
  segments: [],
  startTime: '2026-05-01T10:00:00Z',
  endTime: '2026-05-01T10:05:30Z',
  duration: 330000,
  isRunning: false,
  toolCount: 7,
};
html = as.renderTurnBanner(cnGroup, 0);
assert(html.indexOf('耗时') >= 0, 'compact: has 耗时');
assert(html.indexOf('工作') === -1, 'compact: avoids 工作 wording');
assert(html.indexOf('5m 30s') >= 0, 'Chinese: has duration');
assert(html.indexOf('7 次操作') >= 0, 'Chinese: has action count');

var cnRunning = {
  segments: [],
  startTime: '2026-05-01T10:00:00Z',
  endTime: '2026-05-01T10:00:15Z',
  duration: 15000,
  isRunning: true,
  toolCount: 3,
};
html = as.renderTurnBanner(cnRunning, 1);
assert(html.indexOf('处理中') >= 0, 'Chinese running: has 处理中');
assert(html.indexOf('工作') === -1, 'Chinese running: avoids 工作 wording');
assert(html.indexOf('15s') >= 0, 'Chinese running: has duration');
assert(html.indexOf('3 次操作') >= 0, 'Chinese running: has action count');

// ============================================================
// Summary masking — 折叠摘要不泄露敏感值
// ============================================================

console.log('summary masking');

// 单命令摘要含 password
var sensitiveMsgs = [
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'echo password=supersecret123', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
];
segs = as.buildSegments(sensitiveMsgs);
assert(segs[0].summary.indexOf('supersecret123') === -1, 'summary mask: password hidden');
assert(segs[0].summary.indexOf('****') >= 0, 'summary mask: has mask marker');

// 64 位 hex token 摘要
var hexToken = 'deadbeef0123456789abcdef0123456789abcdef0123456789abcdef01234567';
var hexMsgs = [
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'export TOKEN=' + hexToken, timestamp: '2026-05-01T10:00:00Z', seq: 1 },
];
segs = as.buildSegments(hexMsgs);
assert(segs[0].summary.indexOf(hexToken) === -1, 'summary mask: 64-char hex hidden');
assert(segs[0].summary.indexOf('****') >= 0, 'summary mask: hex replaced with ****');

// renderSegmentCard 对敏感摘要也不泄露
var cardHtml = as.renderSegmentCard(segs[0], 0);
assert(cardHtml.indexOf(hexToken) === -1, 'card HTML: 64-char hex not in output');

// secret=xxx 也被 mask
var secretMsgs = [
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'echo secret=mysecretvalue', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
];
segs = as.buildSegments(secretMsgs);
assert(segs[0].summary.indexOf('mysecretvalue') === -1, 'summary mask: secret hidden');

// ============================================================
// classifyTurnPhase — 工作阶段分类
// ============================================================

console.log('classifyTurnPhase');

// 理解任务: 只有文件读取
assertEq(as.classifyTurnPhase([
  { type: 'explore', fileCount: 2, searchCount: 0, commandCount: 0 }
]), 'understand', 'only file reads → understand');

// 理解任务: 只有搜索
assertEq(as.classifyTurnPhase([
  { type: 'explore', fileCount: 0, searchCount: 1, commandCount: 0 }
]), 'understand', 'only search → understand');

// 执行命令: 纯命令
assertEq(as.classifyTurnPhase([
  { type: 'explore', fileCount: 0, searchCount: 0, commandCount: 2 }
]), 'execute', 'only commands → execute');

// 检查状态: 命令 + 文件
assertEq(as.classifyTurnPhase([
  { type: 'explore', fileCount: 1, searchCount: 0, commandCount: 1 }
]), 'diagnose', 'file + command → diagnose');

// 修改文件: 纯编辑
assertEq(as.classifyTurnPhase([
  { type: 'edit', editCount: 2, fileCount: 1 }
]), 'edit', 'only edits → edit');

// 验证结果: 编辑 + 命令
assertEq(as.classifyTurnPhase([
  { type: 'edit', editCount: 1, fileCount: 1 },
  { type: 'explore', fileCount: 0, searchCount: 0, commandCount: 1 }
]), 'verify', 'edit + command → verify');

// 修改文件: 编辑 + 文件读取（无命令）
assertEq(as.classifyTurnPhase([
  { type: 'explore', fileCount: 1, searchCount: 0, commandCount: 0 },
  { type: 'edit', editCount: 1, fileCount: 1 }
]), 'edit', 'edit + file reads (no command) → edit');

// 空段
assertEq(as.classifyTurnPhase([]), '', 'empty → empty');

// ============================================================
// Integration: renderTurnBanner 包含工作阶段
// ============================================================

console.log('renderTurnBanner includes phase');
var exploreSegs = as.buildSegments([
  { role: 'user', text: 'look at this', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'src/main.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Grep', tool_input_preview: 'TODO', timestamp: '2026-05-01T10:00:02Z', seq: 3 },
]);
var exploreGroups = as.buildTurnGroups(exploreSegs, false);
html = as.renderTurnBanner(exploreGroups[0], 0);
assert(html.indexOf('耗时') >= 0, 'explore turn: has compact duration label');
assert(html.indexOf('工作') === -1, 'explore turn: avoids 工作 wording');

var editPhaseSegs = as.buildSegments([
  { role: 'user', text: 'fix it', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'main.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
]);
var editGroups = as.buildTurnGroups(editPhaseSegs, false);
html = as.renderTurnBanner(editGroups[0], 0);
assert(html.indexOf('耗时') >= 0, 'edit turn: has compact duration label');

var verifySegs = as.buildSegments([
  { role: 'user', text: 'fix and test', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'main.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'cargo test', timestamp: '2026-05-01T10:00:05Z', seq: 3 },
]);
var verifyGroups = as.buildTurnGroups(verifySegs, false);
html = as.renderTurnBanner(verifyGroups[0], 0);
assert(html.indexOf('耗时') >= 0, 'edit+command turn: has compact duration label');

// ============================================================
// Integration: no awkward "工作" wording in turn output
// ============================================================

console.log('integration: compact Chinese banners');
var integrationMsgs = [
  { role: 'user', text: 'fix bug', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'src/lib.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'src/lib.rs', timestamp: '2026-05-01T10:00:05Z', seq: 3 },
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'cargo test', timestamp: '2026-05-01T10:00:10Z', seq: 4 },
];
var intSegs = as.buildSegments(integrationMsgs);
var intGroups = as.buildTurnGroups(intSegs, false);
html = as.renderTurnBanner(intGroups[0], 0);
assert(html.indexOf('Worked for') === -1, 'integration: no "Worked for"');
assert(html.indexOf('Ran ') === -1, 'integration: no "Ran "');
assert(html.indexOf('工作') === -1, 'integration: no 工作 wording');
assert(html.indexOf('耗时') >= 0, 'integration: has 耗时 label');
assert(html.indexOf('次操作') >= 0, 'integration: has 次操作');

// turnBannerLabel 也不含英文
var labelHtml = as.turnBannerLabel(intGroups[0]);
assert(labelHtml.indexOf('Worked for') === -1, 'turnBannerLabel: no "Worked for"');
assert(labelHtml.indexOf('Ran ') === -1, 'turnBannerLabel: no "Ran "');
assert(labelHtml.indexOf('工作') === -1, 'turnBannerLabel: no 工作 wording');
assert(labelHtml.indexOf('耗时') >= 0, 'turnBannerLabel: has 耗时 label');
assert(labelHtml.indexOf('次操作') >= 0, 'turnBannerLabel: has 次操作');

// ============================================================
// buildToolRuns — 基本切分
// ============================================================

console.log('buildToolRuns basic');
var trMsgs = [
  { role: 'user', text: 'fix bug', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'b.rs', timestamp: '2026-05-01T10:00:02Z', seq: 3 },
  { role: 'tool_summary', tool_name: 'Grep', tool_input_preview: 'TODO', timestamp: '2026-05-01T10:00:03Z', seq: 4 },
  { role: 'assistant', text: 'found it', timestamp: '2026-05-01T10:00:04Z', seq: 5 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:05Z', seq: 6 },
  { role: 'assistant', text: 'done', timestamp: '2026-05-01T10:00:06Z', seq: 7 },
];
var trSegs = as.buildSegments(trMsgs);
var trGroups = as.buildTurnGroups(trSegs, false);
assertEq(trGroups.length, 1, 'one turn group');
var runs = as.buildToolRuns(trGroups[0]);
assertEq(runs.length, 2, 'two tool runs (split by assistant)');

// First run: Read, Read, Grep → 3 tools
assertEq(runs[0].toolCount, 3, 'run 1: 3 tools');
assertEq(runs[0].duration, 2000, 'run 1: 2s duration (10:00:01 → 10:00:03)');
assert(!runs[0].isRunning, 'run 1: not running');

// Second run: Edit → 1 tool
assertEq(runs[1].toolCount, 1, 'run 2: 1 tool');
assertEq(runs[1].duration, 0, 'run 2: 0 duration (single item)');
assert(!runs[1].isRunning, 'run 2: not running');

// ============================================================
// buildToolRuns — 每个 run 独立计数，不显示总数
// ============================================================

console.log('buildToolRuns per-run counts');
var label1 = as.turnBannerLabel(runs[0]);
assert(label1.indexOf('3 次操作') >= 0, 'run 1 banner: shows 3 次操作');
assert(label1.indexOf('4') === -1, 'run 1 banner: no total 4');

var label2 = as.turnBannerLabel(runs[1]);
assert(label2.indexOf('1 次操作') >= 0, 'run 2 banner: shows 1 次操作');

// ============================================================
// buildToolRuns — 独立阶段标签
// ============================================================

console.log('buildToolRuns phase labels');
assert(label1.indexOf('耗时') >= 0, 'run 1: compact duration label');
assert(label2.indexOf('耗时') >= 0, 'run 2: compact duration label');

// ============================================================
// buildToolRuns — 空 group / 无工具
// ============================================================

console.log('buildToolRuns edge cases');
var emptyGroup = { segments: [], startTime: null, endTime: null, duration: 0, isRunning: false, toolCount: 0 };
assertEq(as.buildToolRuns(emptyGroup).length, 0, 'empty group → 0 runs');

var chatOnlyGroup = {
  segments: [
    { type: 'assistant', message: { text: 'hi' } },
    { type: 'assistant', message: { text: 'bye' } },
  ],
  startTime: null, endTime: null, duration: 0, isRunning: false, toolCount: 0
};
assertEq(as.buildToolRuns(chatOnlyGroup).length, 0, 'chat-only group → 0 runs');

// ============================================================
// buildToolRuns — 无 assistant 切分（连续工具）
// ============================================================

console.log('buildToolRuns single run');
var singleRunMsgs = [
  { role: 'user', text: 'go', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Bash', tool_input_preview: 'cargo test', timestamp: '2026-05-01T10:00:05Z', seq: 3 },
];
var srSegs = as.buildSegments(singleRunMsgs);
var srGroups = as.buildTurnGroups(srSegs, false);
var srRuns = as.buildToolRuns(srGroups[0]);
assertEq(srRuns.length, 1, 'no assistant → single run');
assertEq(srRuns[0].toolCount, 2, 'single run: 2 tools');
assertEq(srRuns[0].duration, 4000, 'single run: 4s');

// ============================================================
// buildToolRuns — isRunning 继承
// ============================================================

console.log('buildToolRuns isRunning');
var runningMsgs = [
  { role: 'user', text: 'go', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
];
var runSegs2 = as.buildSegments(runningMsgs);
var runGroups2 = as.buildTurnGroups(runSegs2, true);
var runRuns2 = as.buildToolRuns(runGroups2[0]);
assertEq(runRuns2.length, 1, 'running: single run');
assert(runRuns2[0].isRunning, 'running: run inherits isRunning');

// ============================================================
// buildToolRuns — renderTurnBanner 默认 display:none
// ============================================================

console.log('buildToolRuns render defaults');
var banner1 = as.renderTurnBanner(runs[0], 'tr0');
assert(banner1.indexOf('display:none') >= 0, 'run 1 banner: body is display:none');
assert(banner1.indexOf('3 次操作') >= 0, 'run 1 banner: has 3 次操作');

var banner2 = as.renderTurnBanner(runs[1], 'tr1');
assert(banner2.indexOf('display:none') >= 0, 'run 2 banner: body is display:none');
assert(banner2.indexOf('1 次操作') >= 0, 'run 2 banner: has 1 次操作');

// ============================================================
// buildToolRuns — user→3 tools→assistant→1 tool→assistant 完整场景
// ============================================================

console.log('buildToolRuns full scenario: correct per-run counts');
var fullMsgs = [
  { role: 'user', text: 'fix', timestamp: '2026-05-01T10:00:00Z', seq: 1 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:01Z', seq: 2 },
  { role: 'tool_summary', tool_name: 'Read', tool_input_preview: 'b.rs', timestamp: '2026-05-01T10:00:02Z', seq: 3 },
  { role: 'tool_summary', tool_name: 'Grep', tool_input_preview: 'TODO', timestamp: '2026-05-01T10:00:03Z', seq: 4 },
  { role: 'assistant', text: 'found', timestamp: '2026-05-01T10:00:04Z', seq: 5 },
  { role: 'tool_summary', tool_name: 'Edit', tool_input_preview: 'a.rs', timestamp: '2026-05-01T10:00:05Z', seq: 6 },
  { role: 'assistant', text: 'done', timestamp: '2026-05-01T10:00:06Z', seq: 7 },
];
var fSegs = as.buildSegments(fullMsgs);
var fGroups = as.buildTurnGroups(fSegs, false);
var fRuns = as.buildToolRuns(fGroups[0]);

// Must be 2 runs, not showing totals
assertEq(fRuns.length, 2, 'full: 2 runs');
assertEq(fRuns[0].toolCount, 3, 'full: run 1 has 3 tools');
assertEq(fRuns[1].toolCount, 1, 'full: run 2 has 1 tool');

// Banner labels show per-run counts
var fl1 = as.turnBannerLabel(fRuns[0]);
var fl2 = as.turnBannerLabel(fRuns[1]);
assert(fl1.indexOf('3 次操作') >= 0, 'full: run 1 shows 3 次操作');
assert(fl2.indexOf('1 次操作') >= 0, 'full: run 2 shows 1 次操作');
// Must NOT show total 4
assert(fl1.indexOf('4') === -1, 'full: run 1 does not show total 4');
assert(fl2.indexOf('4') === -1, 'full: run 2 does not show total 4');

// Both bodies display:none
var fb1 = as.renderTurnBanner(fRuns[0], 'f0');
var fb2 = as.renderTurnBanner(fRuns[1], 'f1');
assert(fb1.indexOf('display:none') >= 0, 'full: run 1 body hidden');
assert(fb2.indexOf('display:none') >= 0, 'full: run 2 body hidden');

// ============================================================
// 结果
// ============================================================

console.log('\n' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
