/* approval_review.js — 审批 review payload 统一渲染器 */

/// 渲染 review payload 为标准卡片。无 review 时 fallback 到旧字段。
function renderApprovalReview(e) {
  var r = e.review;
  if (!r) return renderLegacyPendingMeta(e);

  var parts = '';

  // chips (agent, tool, rule)
  if (r.chips && r.chips.length) {
    parts += '<div class="review-chips">' + r.chips.map(function(c) {
      return '<span class="review-chip">' + esc(c) + '</span>';
    }).join('') + '</div>';
  }

  // risk_reason
  if (r.risk_reason) {
    parts += '<div class="review-risk">' + esc(r.risk_reason) + '</div>';
  }

  // command
  if (r.command) {
    parts += '<div class="review-command">' + esc(r.command) + '</div>';
  }

  // file_path
  if (r.file_path) {
    parts += '<div class="review-meta">' + esc(r.file_path) + '</div>';
  }

  // payload preview
  if (r.payload_preview) {
    parts += '<div class="review-payload-preview">' + esc(r.payload_preview) + '</div>';
  }

  // device
  if (r.device_label) {
    parts += '<span class="review-device">' + esc(r.device_label) + '</span>';
  }

  return parts;
}

/// 紧凑模式渲染（只显示 tool_name + 一行摘要）。
function renderApprovalReviewCompact(e) {
  var r = e.review;
  if (!r) return renderLegacyPendingMeta(e);
  var parts = [esc(e.tool_name || '-')];
  if (r.command) parts.push(esc(r.command));
  else if (r.file_path) parts.push(esc(String(r.file_path).split('/').pop()));
  return parts.join(' · ');
}

/// 旧字段 fallback：file_path + rule_id。
function renderLegacyPendingMeta(e) {
  var meta = [];
  if (e.file_path) meta.push(esc(e.file_path.split('/').pop()));
  if (e.rule_id) meta.push(esc(e.rule_id));
  return meta.length ? '<div class="pending-card-meta">' + meta.join(' · ') + '</div>' : '';
}
