// provider_capabilities_test.js — Provider capability 展示原语测试

const { providerCapabilityLabels, providerCapabilityText } = require('../provider_capabilities.js');

let passed = 0;
let failed = 0;

function assertEqual(actual, expected, label) {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  if (ok) passed++;
  else {
    failed++;
    console.error('  FAIL: ' + label);
    console.error('    expected: ' + JSON.stringify(expected));
    console.error('    actual:   ' + JSON.stringify(actual));
  }
}

console.log('providerCapabilityLabels');

(function test_full_capabilities() {
  assertEqual(providerCapabilityLabels({
    supports_resume: true,
    supports_transcript: true,
    supports_pretooluse: true,
    supports_posttooluse: true,
    supports_stop: true,
    supports_native_timeout: true,
    supports_permission_passthrough: true,
  }), ['可继续', 'Transcript', 'Before', 'After', 'Stop', 'Native timeout', 'Full Access'], 'full capabilities');
})();

(function test_minimal_capabilities() {
  assertEqual(providerCapabilityLabels({}), ['仅新建'], 'minimal capabilities');
})();

(function test_text() {
  assertEqual(providerCapabilityText({
    supports_resume: true,
    supports_stop: true,
  }), '可继续 · Stop', 'capability text');
})();

console.log('\n' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
