/* approval_review_test.js — unit tests for approval_review.js */

// Minimal stubs for esc / badge
function esc(s) { return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;'); }
function badge() { return ''; }

// Load the module under test
eval(require('fs').readFileSync('crates/shared_ui/approval_review.js', 'utf8'));

var passed = 0, failed = 0;
function assert(cond, msg) { if (cond) passed++; else { failed++; console.error('FAIL:', msg); } }

// Test 1: renderApprovalReview with full review payload
var e1 = {
  tool_name: 'Bash',
  rule_id: 'D001',
  review: {
    view: 'standard',
    summary: 'Bash 需要审批',
    risk_reason: 'git push to main/master -> ask',
    agent: 'Claude Code',
    tool_name: 'Bash',
    file_path: null,
    command: 'git push origin main',
    device_label: 'MacBook',
    payload_preview: null,
    chips: ['Claude Code', 'Bash', 'D001']
  }
};
var html1 = renderApprovalReview(e1);
assert(html1.indexOf('review-chips') >= 0, 'review chips rendered');
assert(html1.indexOf('review-risk') >= 0, 'risk_reason rendered');
assert(html1.indexOf('git push to main/master -&gt; ask') >= 0, 'risk_reason content present');
assert(html1.indexOf('review-command') >= 0, 'review command rendered');
assert(html1.indexOf('git push origin main') >= 0, 'command content present');
assert(html1.indexOf('review-device') >= 0, 'device label rendered');

// Test 2: renderApprovalReview without review (fallback)
var e2 = { tool_name: 'Edit', file_path: '/src/main.rs', rule_id: 'D002' };
var html2 = renderApprovalReview(e2);
assert(html2.indexOf('pending-card-meta') >= 0, 'fallback meta rendered');
assert(html2.indexOf('main.rs') >= 0, 'file path basename shown');

// Test 3: renderApprovalReviewCompact
var html3 = renderApprovalReviewCompact(e1);
assert(html3.indexOf('Bash') >= 0, 'compact shows tool_name');
assert(html3.indexOf('git push origin main') >= 0, 'compact shows command');

// Test 4: compact fallback (renders legacy pending-card-meta)
var html4 = renderApprovalReviewCompact(e2);
assert(html4.indexOf('pending-card-meta') >= 0, 'compact fallback renders legacy meta');
assert(html4.indexOf('main.rs') >= 0, 'compact fallback shows file_path');

// Test 5: no review, no file_path, no rule_id
var e5 = { tool_name: 'Bash' };
var html5 = renderApprovalReview(e5);
assert(html5 === '', 'empty legacy meta when nothing to show');

// Test 6: review with payload_preview visible
var e6 = {
  tool_name: 'Edit',
  review: {
    view: 'full',
    chips: ['Edit'],
    risk_reason: null,
    file_path: '/src/app.tsx',
    command: null,
    device_label: null,
    payload_preview: '{"file_path":"/src/app.tsx","old_string":"foo","new_string":"bar"}'
  }
};
var html6 = renderApprovalReview(e6);
assert(html6.indexOf('review-payload-preview') >= 0, 'payload_preview rendered');
assert(html6.indexOf('/src/app.tsx') >= 0, 'payload_preview content present');
assert(html6.indexOf('review-meta') >= 0, 'file_path rendered via review');

// Test 7: review with risk_reason but no other details
var e7 = {
  tool_name: 'Bash',
  review: {
    view: 'compact',
    chips: ['Bash'],
    risk_reason: 'D011: pipe to shell -> ask',
    file_path: null,
    command: null,
    device_label: null,
    payload_preview: null
  }
};
var html7 = renderApprovalReview(e7);
assert(html7.indexOf('review-risk') >= 0, 'risk_reason rendered in minimal review');
assert(html7.indexOf('D011') >= 0, 'risk_reason content shown');

console.log('approval_review: ' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
