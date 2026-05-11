// provider_capabilities.js — Provider capability 展示原语
//
// 职责：把后端 ProviderCapabilities 转成 Bridge / Relay 共用的人类可读标签。

(function (root) {
  function providerCapabilityLabels(caps) {
    caps = caps || {};
    const labels = [];
    if (caps.supports_resume) labels.push('可继续');
    else labels.push('仅新建');
    if (caps.supports_transcript) labels.push('Transcript');
    if (caps.supports_pretooluse) labels.push('Before');
    if (caps.supports_posttooluse) labels.push('After');
    if (caps.supports_stop) labels.push('Stop');
    if (caps.supports_native_timeout) labels.push('Native timeout');
    if (caps.supports_permission_passthrough) labels.push('Full Access');
    return labels;
  }

  function providerCapabilityText(caps) {
    return providerCapabilityLabels(caps).join(' · ');
  }

  root.providerCapabilityLabels = providerCapabilityLabels;
  root.providerCapabilityText = providerCapabilityText;

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { providerCapabilityLabels, providerCapabilityText };
  }
})(typeof globalThis !== 'undefined' ? globalThis : this);
