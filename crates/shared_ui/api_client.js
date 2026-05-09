// api_client.js — 共享 HTTP 客户端层
//
// 职责：统一 GET/POST 请求、token 注入、错误解析、409 runtime_drift 处理。
// bridge 和 relay 不再各写 api() / fetchJSON()，统一使用此模块。
//
// 设计决策：
// - 使用 async/await（比 promise chain 更清晰）
// - 返回原始 Response（调用方按需 .json() 或 .text()）
// - 错误时 throw 结构化对象 { code, message, ... }
// - token 通过全局 S.token 获取（bridge 和 relay 都用 S 对象存 token）
//
// 环境兼容：
// - 浏览器：被 ui.rs / mobile_ui.rs include_str! 后注入 <script>，函数挂全局
// - Node.js：通过 module.exports 导出，供测试 require（需 mock fetch）

// ============================================================
// API 请求
// ============================================================

/**
 * 发送 HTTP 请求，自动注入 Authorization header。
 *
 * @param {string} path - 请求路径（如 '/api/health'）
 * @param {object} [opts={}] - fetch 选项（method, body, headers 等）
 * @returns {Promise<Response>} 原始 Response 对象
 * @throws {object} 结构化错误 { code, message, runtime_health? }
 *
 * 错误码：
 * - network_error: 网络连接失败
 * - auth_failed: Token 无效或已过期
 * - mac_offline: Mac 不在线（503）
 * - runtime_drift: 运行环境漂移（409 + runtime_health）
 * - request_failed: 其他请求失败
 */
async function api(path, opts) {
  opts = opts || {};
  var headers = {
    'Authorization': 'Bearer ' + (typeof S !== 'undefined' ? S.token : ''),
    'Content-Type': 'application/json',
  };
  // 合并调用方 headers
  if (opts.headers) {
    var keys = Object.keys(opts.headers);
    for (var i = 0; i < keys.length; i++) {
      headers[keys[i]] = opts.headers[keys[i]];
    }
  }
  console.debug('[api] %s %s', opts.method || 'GET', path);
  var res;
  try {
    res = await fetch(path, { method: opts.method, body: opts.body, headers: headers });
  } catch (e) {
    console.debug('[api] network error:', e.message);
    throw { code: 'network_error', message: '网络连接失败' };
  }
  console.debug('[api] %s -> %d', path, res.status);
  // 401/403 — 认证失败
  if (res.status === 401 || res.status === 403) {
    var data = await res.json().catch(function () { return {}; });
    var err = data.error || '';
    console.debug('[api] auth/forbidden:', err || res.status);
    if (err === 'sid_not_registered') {
      throw { code: 'auth_failed', message: '配对已失效，请重新连接' };
    }
    if (err === 'token_revoked') {
      throw { code: 'auth_failed', message: 'Token 已被轮换，请重新连接' };
    }
    if (err === 'wrong_token_role') {
      throw { code: 'auth_failed', message: 'Token 类型错误，请使用 Client Token' };
    }
    if (err === 'endpoint not allowed') {
      throw { code: 'request_failed', message: '该接口暂未开放' };
    }
    throw { code: 'auth_failed', message: err || 'Token 无效或已过期' };
  }
  // 503 — Mac 不在线
  if (res.status === 503) {
    var data503 = await res.json().catch(function () { return {}; });
    console.debug('[api] mac_offline:', data503.error);
    throw { code: 'mac_offline', message: data503.error || 'Mac 不在线' };
  }
  // 409 — 运行环境漂移
  if (res.status === 409) {
    var data409 = await res.json().catch(function () { return {}; });
    if (data409.runtime_health) {
      throw {
        code: 'runtime_drift',
        message: data409.message || '运行环境已漂移',
        runtime_health: data409.runtime_health,
      };
    }
    if (data409.cost_stats) {
      throw {
        code: 'resume_cost',
        message: data409.message || '继续会话成本过高',
        cost_stats: data409.cost_stats,
      };
    }
    throw { code: 'request_failed', message: data409.error || '冲突 (' + res.status + ')' };
  }
  // 其他非 2xx
  if (!res.ok) {
    var text = await res.text().catch(function () { return ''; });
    var dataErr = {};
    try { dataErr = text ? JSON.parse(text) : {}; } catch (_) {}
    console.debug('[api] request_failed: %d %s', res.status, text);
    throw { code: 'request_failed', message: dataErr.error || '请求失败 (' + res.status + ')' };
  }
  return res;
}

// ============================================================
// 便捷方法
// ============================================================

/**
 * GET 请求并返回解析后的 JSON。
 *
 * @param {string} path
 * @returns {Promise<any>}
 */
async function apiJson(path) {
  var res = await api(path);
  return res.json();
}

/**
 * POST JSON 请求并返回解析后的 JSON。
 *
 * @param {string} path
 * @param {object} body
 * @returns {Promise<any>}
 */
async function apiPost(path, body) {
  var res = await api(path, { method: 'POST', body: JSON.stringify(body) });
  return res.json();
}

// ============================================================
// 导出
// ============================================================
if (typeof module !== 'undefined' && module.exports) {
  module.exports = { api: api, apiJson: apiJson, apiPost: apiPost };
}
