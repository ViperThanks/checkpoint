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
assert(segs[1].summary.indexOf('2 files') >= 0, 'summary has 2 files');
assert(segs[1].summary.indexOf('1 search') >= 0, 'summary has 1 search');
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
assert(segs[0].summary.indexOf('1 file') >= 0, 'summary: 1 file');
assert(segs[0].summary.indexOf('1 command') >= 0, 'summary: 1 command');
assert(segs[0].summary.indexOf('1 search') >= 0, 'summary: 1 search');
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
assert(segs[0].summary.indexOf('2 edits') >= 0, 'edit summary');

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
assert(segs[2].summary.indexOf('1 file') >= 0, 'full: 1 file');
assert(segs[2].summary.indexOf('1 search') >= 0, 'full: 1 search');
assert(segs[2].summary.indexOf('1 command') >= 0, 'full: 1 command');
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
  summary: '2 files · 1 search',
  startTime: null, endTime: null, duration: 0,
  fileCount: 2, searchCount: 1, commandCount: 0, editCount: 0,
};
var html = as.renderSegmentCard(cardSeg, 0);
assert(html.indexOf('act-card') >= 0, 'has act-card class');
assert(html.indexOf('act-explore') >= 0, 'has act-explore class');
assert(html.indexOf('2 files') >= 0, 'summary text in HTML');
assert(html.indexOf('toggleActDetail') >= 0, 'has toggle for 2+ items');
assert(html.indexOf('toggleActDetail(this)') >= 0, 'segment toggle is element-local');
assert(html.indexOf('act-detail') >= 0, 'has detail div');

// Single item — no toggle
var singleSeg = {
  type: 'edit',
  items: [{ tool_name: 'Edit', tool_input_preview: 'a.rs' }],
  summary: '1 edit',
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
assert(detail.indexOf('Read') >= 0, 'detail: has Read');
assert(detail.indexOf('Grep') >= 0, 'detail: has Grep');
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
assert(html.indexOf('Worked for') >= 0, 'settled: has Worked for');
assert(html.indexOf('3m 21s') >= 0, 'settled: has duration');
assert(html.indexOf('Ran 2 commands') >= 0, 'settled: has command count');
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
assert(html.indexOf('Working') >= 0, 'running: has Working');
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
assert(codexCmdSegs[0].summary.indexOf('2 commands') >= 0, 'Codex summary uses 2 commands');

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
// 结果
// ============================================================

console.log('\n' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
