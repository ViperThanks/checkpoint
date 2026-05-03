const { invoke } = window.__TAURI__.core;

const $loading = document.getElementById('loading');
const $diagnostics = document.getElementById('diagnostics');
const $redirecting = document.getElementById('redirecting');
const $diagGrid = document.getElementById('diag-grid');
const $btnStart = document.getElementById('btn-start');
const $btnRetry = document.getElementById('btn-retry');
const $errorMsg = document.getElementById('error-msg');

function showScreen(screen) {
  $loading.style.display = 'none';
  $diagnostics.style.display = 'none';
  $redirecting.style.display = 'none';
  screen.style.display = 'flex';
}

function renderDiagnostics(diag) {
  const fields = [
    ['Data Directory', diag.data_dir],
    ['Binary Path', diag.binary_path],
    ['Bridge Status', diag.bridge_status],
    ['PID', diag.pid || ''],
    ['Address', diag.addr || ''],
    ['LAN', diag.lan_enabled ? 'enabled' : 'disabled'],
    ['Launchd', diag.launchd_loaded ? 'loaded' : 'not loaded'],
    ['Keep-awake', diag.keep_awake ? 'enabled' : 'disabled'],
    ['Token Path', diag.token_path],
    ['Log File', diag.log_file],
    ['Audit DB', diag.audit_db],
  ];
  $diagGrid.innerHTML = fields
    .map(([label, value]) => `<span class="label">${label}</span><span class="value">${value || '-'}</span>`)
    .join('');
}

async function checkAndRedirect() {
  showScreen($loading);

  try {
    const status = await invoke('check_bridge_status');

    if (status.is_running) {
      const url = await invoke('get_bridge_url');
      if (url) {
        showScreen($redirecting);
        // Navigate the Tauri webview to the bridge URL
        window.location.href = url;
        return;
      }
    }

    // Bridge not running — show diagnostics
    const diag = await invoke('get_diagnostics');
    renderDiagnostics(diag);
    if (status.error) {
      $errorMsg.textContent = status.error;
      $errorMsg.style.display = 'block';
    }
    showScreen($diagnostics);
  } catch (e) {
    $errorMsg.textContent = String(e);
    $errorMsg.style.display = 'block';
    showScreen($diagnostics);
  }
}

$btnStart.addEventListener('click', async () => {
  $btnStart.disabled = true;
  $btnStart.textContent = 'Starting...';
  try {
    const status = await invoke('start_bridge');
    if (status.is_running) {
      const url = await invoke('get_bridge_url');
      if (url) {
        showScreen($redirecting);
        window.location.href = url;
        return;
      }
    }
    // Still not running — refresh diagnostics
    const diag = await invoke('get_diagnostics');
    renderDiagnostics(diag);
    $errorMsg.textContent = status.error || 'Bridge failed to start';
    $errorMsg.style.display = 'block';
  } catch (e) {
    $errorMsg.textContent = String(e);
    $errorMsg.style.display = 'block';
  } finally {
    $btnStart.disabled = false;
    $btnStart.textContent = 'Start Bridge';
  }
});

$btnRetry.addEventListener('click', () => {
  checkAndRedirect();
});

// Initial check
checkAndRedirect();
