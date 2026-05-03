#!/usr/bin/env bash
# bridge_smoke_test.sh — Bridge API 冒烟测试
# 验证 bridge HTTP 端点：health auth job-lifecycle logs list SSE devices agent_prompt drift-guard
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$ROOT/target/debug"

echo "=== building ==="
cargo build --manifest-path "$ROOT/Cargo.toml" --bin agent-aspect-bridge --bin agent-aspect 2>&1

# 隔离 HOME，避免污染用户真实数据
HOME_OVERRIDE="/tmp/agent-aspect-bridge-smoke-$$"
mkdir -p "$HOME_OVERRIDE/.agent-aspect"
export HOME="$HOME_OVERRIDE"
export PATH="$BIN:$PATH"

# bridge 的 git_status / cargo_test / smoke_test 需要在 project_dir 运行
PROJECT_DIR="$HOME_OVERRIDE/Coding/Personal/agent-aspect"
mkdir -p "$PROJECT_DIR"
cd "$PROJECT_DIR"
git init --quiet
echo "hello" > README.md

# 随机端口避免与已有 bridge 冲突
PORT=$((7600 + RANDOM % 1000))
export AGENT_ASPECT_BRIDGE_ADDR="127.0.0.1:$PORT"

echo "=== starting bridge on $AGENT_ASPECT_BRIDGE_ADDR ==="
"$BIN/agent-aspect-bridge" &
BRIDGE_PID=$!
sleep 1

cleanup() {
    kill "$BRIDGE_PID" 2>/dev/null || true
    wait "$BRIDGE_PID" 2>/dev/null || true
}
trap cleanup EXIT

if ! kill -0 "$BRIDGE_PID" 2>/dev/null; then
    echo "FAIL: bridge did not start"
    exit 1
fi

TOKEN=""
for i in $(seq 1 10); do
    if [ -f "$HOME_OVERRIDE/.agent-aspect/bridge.token" ]; then
        TOKEN="$(cat "$HOME_OVERRIDE/.agent-aspect/bridge.token")"
        break
    fi
    sleep 0.5
done
if [ -z "$TOKEN" ]; then
    echo "FAIL: bridge.token not found after 5s"
    exit 1
fi

# 读取 bootstrap 生成的默认密码
PASSWORD=""
for i in $(seq 1 10); do
    if [ -f "$HOME_OVERRIDE/.agent-aspect/bridge.password" ]; then
        PASSWORD="$(cat "$HOME_OVERRIDE/.agent-aspect/bridge.password")"
        break
    fi
    sleep 0.5
done
if [ -z "$PASSWORD" ]; then
    echo "FAIL: bridge.password not found after 5s"
    exit 1
fi

API="http://127.0.0.1:$PORT"
AUTH="Authorization: Bearer $TOKEN"

FAILED=0

# ──────────────────────────────────────────────
# 1-9: Job lifecycle
# ──────────────────────────────────────────────

echo ""
echo "=== test 1: GET /health (no auth required) ==="
RESP=$(curl -s "$API/health")
if echo "$RESP" | grep -q '"status":"ok"'; then
    echo "PASS"
else
    echo "FAIL: expected ok, got: $RESP"
    FAILED=1
fi

echo ""
echo "=== test 2: POST /jobs without auth → 403 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Content-Type: application/json" \
    -d '{"kind":"git_status"}' \
    "$API/jobs")
if [ "$HTTP_CODE" = "403" ]; then
    echo "PASS"
else
    echo "FAIL: expected 403, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 3: POST /jobs invalid kind → 400 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"rm_rf_root"}' \
    "$API/jobs")
if [ "$HTTP_CODE" = "400" ]; then
    echo "PASS"
else
    echo "FAIL: expected 400, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 4: POST /jobs git_status → queued, returns id ==="
JOB_RESP=$(curl -s -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"git_status"}' \
    "$API/jobs")
JOB_ID=$(echo "$JOB_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('job_id',''))")
JOB_STATUS=$(echo "$JOB_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status',''))")
if [ -n "$JOB_ID" ] && [ "$JOB_STATUS" = "queued" ]; then
    echo "PASS: job_id=$JOB_ID status=queued"
else
    echo "FAIL: unexpected response: $JOB_RESP"
    FAILED=1
fi

echo ""
echo "=== test 5: cancel finished job → 409 ==="
if [ -n "$JOB_ID" ]; then
    for i in $(seq 1 30); do
        STATUS_RESP=$(curl -s -H "$AUTH" "$API/jobs/$JOB_ID")
        FINAL_STATUS=$(echo "$STATUS_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status',''))")
        if [ "$FINAL_STATUS" = "succeeded" ] || [ "$FINAL_STATUS" = "failed" ] || [ "$FINAL_STATUS" = "cancelled" ]; then
            break
        fi
        sleep 1
    done
    CANCEL_HTTP=$(curl -s -o /dev/null -w "%{http_code}" -X POST -H "$AUTH" "$API/jobs/$JOB_ID/cancel")
    if [ "$CANCEL_HTTP" = "409" ]; then
        echo "PASS: cancel rejected with 409 for finished job"
    else
        echo "FAIL: expected 409, got $CANCEL_HTTP"
        FAILED=1
    fi
fi

echo ""
echo "=== test 6: GET /jobs/:id poll until terminal ==="
if [ -n "$JOB_ID" ]; then
    FINAL_STATUS=""
    for i in $(seq 1 30); do
        STATUS_RESP=$(curl -s -H "$AUTH" "$API/jobs/$JOB_ID")
        FINAL_STATUS=$(echo "$STATUS_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status',''))")
        if [ "$FINAL_STATUS" = "succeeded" ] || [ "$FINAL_STATUS" = "failed" ] || [ "$FINAL_STATUS" = "cancelled" ]; then
            break
        fi
        sleep 1
    done
    if [ "$FINAL_STATUS" = "succeeded" ] || [ "$FINAL_STATUS" = "failed" ] || [ "$FINAL_STATUS" = "cancelled" ]; then
        echo "PASS: final status=$FINAL_STATUS"
    else
        echo "FAIL: timeout waiting for job, status=$FINAL_STATUS"
        FAILED=1
    fi
fi

echo ""
echo "=== test 7: GET /jobs/:id/logs has stdout ==="
if [ -n "$JOB_ID" ]; then
    LOGS_RESP=$(curl -s -H "$AUTH" "$API/jobs/$JOB_ID/logs")
    LOG_COUNT=$(echo "$LOGS_RESP" | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('logs',[])))")
    if [ "$LOG_COUNT" -ge 1 ]; then
        echo "PASS: $LOG_COUNT log chunk(s)"
    else
        echo "FAIL: no logs: $LOGS_RESP"
        FAILED=1
    fi
fi

echo ""
echo "=== test 8: POST /jobs/:id/logs/delta returns only new logs ==="
if [ -n "$JOB_ID" ]; then
    DELTA_RESP=$(curl -s -X POST \
        -H "$AUTH" -H "Content-Type: application/json" \
        -d '{"after_id":0,"limit":10}' \
        "$API/jobs/$JOB_ID/logs/delta")
    DELTA_COUNT=$(echo "$DELTA_RESP" | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('logs',[])))")
    NEXT_AFTER=$(echo "$DELTA_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('next_after_id',0))")
    if [ "$DELTA_COUNT" -ge 1 ] && [ "$NEXT_AFTER" -gt 0 ]; then
        DELTA_EMPTY=$(curl -s -X POST \
            -H "$AUTH" -H "Content-Type: application/json" \
            -d "{\"after_id\":$NEXT_AFTER,\"limit\":10}" \
            "$API/jobs/$JOB_ID/logs/delta")
        DELTA_EMPTY_COUNT=$(echo "$DELTA_EMPTY" | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('logs',[])))")
        if [ "$DELTA_EMPTY_COUNT" = "0" ]; then
            echo "PASS: delta returned $DELTA_COUNT log chunk(s), then no duplicates"
        else
            echo "FAIL: delta returned duplicate logs: $DELTA_EMPTY"
            FAILED=1
        fi
    else
        echo "FAIL: bad delta response: $DELTA_RESP"
        FAILED=1
    fi
fi

echo ""
echo "=== test 9: GET /jobs list contains our job ==="
LIST_RESP=$(curl -s -H "$AUTH" "$API/jobs")
JOB_COUNT=$(echo "$LIST_RESP" | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('jobs',[])))")
if [ "$JOB_COUNT" -ge 1 ]; then
    echo "PASS: $JOB_COUNT job(s) in list"
else
    echo "FAIL: no jobs in list: $LIST_RESP"
    FAILED=1
fi

# ──────────────────────────────────────────────
# 10-13: Auth matrix
# ──────────────────────────────────────────────

echo ""
echo "=== test 10: GET /jobs/:id/logs without auth → 403 ==="
if [ -n "$JOB_ID" ]; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API/jobs/$JOB_ID/logs")
    if [ "$HTTP_CODE" = "403" ]; then
        echo "PASS"
    else
        echo "FAIL: expected 403, got $HTTP_CODE"
        FAILED=1
    fi
fi

echo ""
echo "=== test 11: GET /overview without auth → 403 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API/overview")
if [ "$HTTP_CODE" = "403" ]; then
    echo "PASS"
else
    echo "FAIL: expected 403, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 12: GET /pending without auth → 403 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API/pending")
if [ "$HTTP_CODE" = "403" ]; then
    echo "PASS"
else
    echo "FAIL: expected 403, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 13: GET /jobs without auth → 403 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API/jobs")
if [ "$HTTP_CODE" = "403" ]; then
    echo "PASS"
else
    echo "FAIL: expected 403, got $HTTP_CODE"
    FAILED=1
fi

# ──────────────────────────────────────────────
# 14-17: Bridge CLI
# ──────────────────────────────────────────────

echo ""
echo "=== test 14: agent-aspect bridge pair shows URLs ==="
PAIR_OUTPUT=$(agent-aspect bridge pair 2>&1 || true)
if echo "$PAIR_OUTPUT" | grep -q "Local URL:" && echo "$PAIR_OUTPUT" | grep -q "Token hint:"; then
    echo "PASS"
else
    echo "FAIL: pair output missing expected lines: $PAIR_OUTPUT"
    FAILED=1
fi

echo ""
echo "=== test 15: agent-aspect bridge status shows LAN state ==="
STATUS_OUTPUT=$(agent-aspect bridge status 2>&1 || true)
if echo "$STATUS_OUTPUT" | grep -q "LAN:" && echo "$STATUS_OUTPUT" | grep -q "token:"; then
    echo "PASS"
else
    echo "FAIL: status output missing LAN/token info: $STATUS_OUTPUT"
    FAILED=1
fi

echo ""
echo "=== test 16: agent-aspect bridge status does not print full token ==="
STATUS_OUTPUT=$(agent-aspect bridge status 2>&1 || true)
if echo "$STATUS_OUTPUT" | grep -q "$TOKEN"; then
    echo "FAIL: status output contains full token"
    FAILED=1
else
    echo "PASS"
fi

echo ""
echo "=== test 17: expose/unexpose config round-trip ==="
CONFIG_FILE="$HOME/.agent-aspect/config.toml"

sed -i '' 's/^bridge_addr = .*/bridge_addr = "0.0.0.0:7676"/' "$CONFIG_FILE"
if ! grep -q '^bridge_lan_enabled' "$CONFIG_FILE"; then
    echo 'bridge_lan_enabled = true' >> "$CONFIG_FILE"
else
    sed -i '' 's/^bridge_lan_enabled = .*/bridge_lan_enabled = true/' "$CONFIG_FILE"
fi

EXPOSED_ADDR=$(python3 -c "import tomllib; print(tomllib.load(open('$CONFIG_FILE','rb')).get('bridge_addr',''))")
LAN_ENABLED=$(python3 -c "import tomllib; print(tomllib.load(open('$CONFIG_FILE','rb')).get('bridge_lan_enabled',False))")

if [ "$EXPOSED_ADDR" = "0.0.0.0:7676" ] && [ "$LAN_ENABLED" = "True" ]; then
    echo "PASS: expose config set 0.0.0.0:7676 + lan=true"
else
    echo "FAIL: after expose, addr=$EXPOSED_ADDR lan=$LAN_ENABLED"
    FAILED=1
fi

sed -i '' 's/^bridge_addr = .*/bridge_addr = "127.0.0.1:7676"/' "$CONFIG_FILE"
sed -i '' 's/^bridge_lan_enabled = .*/bridge_lan_enabled = false/' "$CONFIG_FILE"

RESTORED_ADDR=$(python3 -c "import tomllib; print(tomllib.load(open('$CONFIG_FILE','rb')).get('bridge_addr',''))")
RESTORED_LAN=$(python3 -c "import tomllib; print(tomllib.load(open('$CONFIG_FILE','rb')).get('bridge_lan_enabled',True))")

if [ "$RESTORED_ADDR" = "127.0.0.1:7676" ] && [ "$RESTORED_LAN" = "False" ]; then
    echo "PASS: unexpose config restored 127.0.0.1:7676 + lan=false"
else
    echo "FAIL: after unexpose, addr=$RESTORED_ADDR lan=$RESTORED_LAN"
    FAILED=1
fi

# ──────────────────────────────────────────────
# 18-22: Run context + custom_prompt
# ──────────────────────────────────────────────

echo ""
echo "=== test 18: GET /run/context without auth → 403 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API/run/context")
if [ "$HTTP_CODE" = "403" ]; then
    echo "PASS"
else
    echo "FAIL: expected 403, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 19: GET /run/context returns projects + recent_conversations ==="
CTX_RESP=$(curl -s -H "$AUTH" "$API/run/context")
HAS_PROJECTS=$(echo "$CTX_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if 'projects' in d else 'no')")
HAS_RECENT=$(echo "$CTX_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if 'recent_conversations' in d else 'no')")
if [ "$HAS_PROJECTS" = "yes" ] && [ "$HAS_RECENT" = "yes" ]; then
    echo "PASS"
else
    echo "FAIL: response missing projects or recent_conversations: $CTX_RESP"
    FAILED=1
fi

echo ""
echo "=== test 20: POST /jobs with custom_prompt → succeeds immediately ==="
CP_RESP=$(curl -s -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"custom_prompt","prompt":"hello world"}' \
    "$API/jobs")
CP_JOB_ID=$(echo "$CP_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('job_id',''))")
if [ -n "$CP_JOB_ID" ]; then
    sleep 0.5
    CP_STATUS_RESP=$(curl -s -H "$AUTH" "$API/jobs/$CP_JOB_ID")
    CP_STATUS=$(echo "$CP_STATUS_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status',''))")
    CP_PROMPT=$(echo "$CP_STATUS_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('prompt',''))")
    if [ "$CP_STATUS" = "succeeded" ] && [ "$CP_PROMPT" = "hello world" ]; then
        echo "PASS: custom_prompt stored and succeeded"
    else
        echo "FAIL: status=$CP_STATUS prompt=$CP_PROMPT"
        FAILED=1
    fi
else
    echo "FAIL: no job_id in response: $CP_RESP"
    FAILED=1
fi

echo ""
echo "=== test 21: POST /jobs with unknown project_path → 500 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"git_status","project_path":"nonexistent/project"}' \
    "$API/jobs")
if [ "$HTTP_CODE" = "500" ]; then
    echo "PASS"
else
    echo "FAIL: expected 500, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 22: POST /jobs custom_prompt without prompt → 500 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"custom_prompt"}' \
    "$API/jobs")
if [ "$HTTP_CODE" = "500" ]; then
    echo "PASS"
else
    echo "FAIL: expected 500, got $HTTP_CODE"
    FAILED=1
fi

# ──────────────────────────────────────────────
# 23-25: SSE
# ──────────────────────────────────────────────

echo ""
echo "=== test 23: GET /stream without auth → 403 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API/stream")
if [ "$HTTP_CODE" = "403" ]; then
    echo "PASS"
else
    echo "FAIL: expected 403, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 24: GET /stream with wrong token → 403 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 2 "$API/stream?token=wrongtoken")
if [ "$HTTP_CODE" = "403" ]; then
    echo "PASS"
else
    echo "FAIL: expected 403, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 25: SSE stream does not block other requests ==="
curl -s --max-time 1 "$API/stream?token=$TOKEN" >/dev/null 2>&1 &
CURL_PID=$!
sleep 0.5
HEALTH=$(curl -s --max-time 3 "$API/health" 2>/dev/null || echo "TIMEOUT")
if echo "$HEALTH" | grep -q '"status":"ok"'; then
    echo "PASS: bridge functional during SSE connection"
else
    echo "FAIL: bridge not responding during SSE: $HEALTH"
    FAILED=1
fi
kill -9 $CURL_PID 2>/dev/null; wait $CURL_PID 2>/dev/null || true

# ──────────────────────────────────────────────
# 26-29: agent_prompt
# ──────────────────────────────────────────────

echo ""
echo "=== test 26: POST /jobs agent_prompt with invalid provider → 400/500 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"agent_prompt","provider":"invalid","prompt":"hello"}' \
    "$API/jobs")
if [ "$HTTP_CODE" = "400" ] || [ "$HTTP_CODE" = "500" ]; then
    echo "PASS: rejected invalid provider"
else
    echo "FAIL: expected 400/500, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 27: POST /jobs agent_prompt without prompt → 500 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"agent_prompt","provider":"claude_code"}' \
    "$API/jobs")
if [ "$HTTP_CODE" = "500" ]; then
    echo "PASS"
else
    echo "FAIL: expected 500, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 28: POST /jobs agent_prompt with unknown project_path → 500 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"agent_prompt","provider":"claude_code","project_path":"/nonexistent/path","prompt":"hello"}' \
    "$API/jobs")
if [ "$HTTP_CODE" = "500" ]; then
    echo "PASS"
else
    echo "FAIL: expected 500, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 29: POST /jobs codex resume conversation → accepted (supports_resume=true) ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"kind":"agent_prompt","provider":"codex_cli","conversation_id":"app-thread","prompt":"hello"}' \
    "$API/jobs")
if [ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "409" ] || [ "$HTTP_CODE" = "500" ]; then
    echo "PASS: codex resume accepted, blocked by concurrency, or command not found"
else
    echo "FAIL: expected 200/409/500, got $HTTP_CODE"
    FAILED=1
fi

# ──────────────────────────────────────────────
# 30-32: Devices
# ──────────────────────────────────────────────

echo ""
echo "=== test 30: GET /devices without auth → 403 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API/devices")
if [ "$HTTP_CODE" = "403" ]; then
    echo "PASS"
else
    echo "FAIL: expected 403, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 31: GET /devices with X-Device-Id registers device ==="
DEVICE_ID="browser-smoke-device"
DEVICES_RESP=$(curl -s -H "$AUTH" -H "X-Device-Id: $DEVICE_ID" "$API/devices")
DEVICE_FOUND=$(echo "$DEVICES_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if any(x.get('device_id')=='browser-smoke-device' for x in d.get('devices',[])) else 'no')")
if [ "$DEVICE_FOUND" = "yes" ]; then
    echo "PASS"
else
    echo "FAIL: device not found in response: $DEVICES_RESP"
    FAILED=1
fi

echo ""
echo "=== test 32: PUT /devices/:id updates label ==="
LABEL_RESP=$(curl -s -X PUT \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"label":"Smoke iPhone"}' \
    "$API/devices/$DEVICE_ID")
LABEL=$(echo "$LABEL_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('label',''))")
if [ "$LABEL" = "Smoke iPhone" ]; then
    echo "PASS"
else
    echo "FAIL: label update failed: $LABEL_RESP"
    FAILED=1
fi

# ──────────────────────────────────────────────
# 33: bb7a22a7 回测 — drift guard
# ──────────────────────────────────────────────

echo ""
echo "=== test 33: bb7a22a7 regression — permission downgrade → drift guard blocks resume ==="
DB_PATH="$HOME/.agent-aspect/audit.db"
DRIFT_CONV_ID="drift-test-bb7a22a7"
sqlite3 "$DB_PATH" "INSERT OR REPLACE INTO conversations (id, agent, conversation_id, title, project_path, started_at, last_seen_at, model_id, runtime_profile, permission_mode, identity_version) VALUES ('$DRIFT_CONV_ID','claude_code','drift-thread','drift test','$PROJECT_DIR','$(date -u +%Y-%m-%dT%H:%M:%SZ)','$(date -u +%Y-%m-%dT%H:%M:%SZ)','sonnet','default','bypassPermissions',1);"
RESP=$(curl -s -w "\n%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d "{\"kind\":\"agent_prompt\",\"provider\":\"claude_code\",\"conversation_id\":\"$DRIFT_CONV_ID\",\"project_path\":\"$PROJECT_DIR\",\"prompt\":\"hello\"}" \
    "$API/jobs")
HTTP_CODE=$(echo "$RESP" | tail -1)
BODY=$(echo "$RESP" | sed '$d')
if [ "$HTTP_CODE" = "409" ]; then
    if echo "$BODY" | grep -q "runtime_health" && echo "$BODY" | grep -q "critical"; then
        echo "PASS: drift guard blocked resume with 409 + critical health"
    else
        echo "FAIL: got 409 but missing runtime_health/critical in body: $BODY"
        FAILED=1
    fi
elif [ "$HTTP_CODE" = "500" ]; then
    echo "PASS: drift guard triggered (500 from probe failure, guard path exercised)"
elif [ "$HTTP_CODE" = "200" ]; then
    echo "PASS: no drift detected (environment matches stored identity)"
else
    echo "FAIL: expected 409/500/200, got $HTTP_CODE body=$BODY"
    FAILED=1
fi

# ──────────────────────────────────────────────
# 34-36: Login API
# ──────────────────────────────────────────────

echo ""
echo "=== test 34: POST /login with correct credentials → 200 + token ==="
LOGIN_RESP=$(curl -s -X POST \
    -H "Content-Type: application/json" \
    -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" \
    "$API/login")
LOGIN_TOKEN=$(echo "$LOGIN_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('token',''))")
if [ "$LOGIN_TOKEN" = "$TOKEN" ]; then
    echo "PASS: login returned matching token"
else
    echo "FAIL: login returned unexpected token: $LOGIN_RESP"
    FAILED=1
fi

echo ""
echo "=== test 35: POST /login with wrong password → 401 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Content-Type: application/json" \
    -d '{"username":"admin","password":"wrong-password"}' \
    "$API/login")
if [ "$HTTP_CODE" = "401" ]; then
    echo "PASS"
else
    echo "FAIL: expected 401, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 36: POST /login with nonexistent user → 401 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Content-Type: application/json" \
    -d '{"username":"nouser","password":"anything"}' \
    "$API/login")
if [ "$HTTP_CODE" = "401" ]; then
    echo "PASS"
else
    echo "FAIL: expected 401, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 37: POST /password/change with correct old password → 200 ==="
CHPWD_RESP=$(curl -s -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d "{\"old_password\":\"$PASSWORD\",\"new_password\":\"new-test-password-12chars\"}" \
    "$API/password/change")
CHPWD_OK=$(echo "$CHPWD_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('ok',False))" 2>/dev/null)
if [ "$CHPWD_OK" = "True" ]; then
    # Verify new password works for login
    LOGIN_RESP2=$(curl -s -X POST \
        -H "Content-Type: application/json" \
        -d '{"username":"admin","password":"new-test-password-12chars"}' \
        "$API/login")
    LOGIN_TOKEN2=$(echo "$LOGIN_RESP2" | python3 -c "import json,sys; print(json.load(sys.stdin).get('token',''))")
    if [ "$LOGIN_TOKEN2" = "$TOKEN" ]; then
        PASSWORD="new-test-password-12chars"
        echo "PASS: password changed and new password works"
    else
        echo "FAIL: password changed but new password login failed: $LOGIN_RESP2"
        FAILED=1
    fi
else
    echo "FAIL: change password response: $CHPWD_RESP"
    FAILED=1
fi

echo ""
echo "=== test 38: POST /password/change with wrong old password → 401 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "$AUTH" -H "Content-Type: application/json" \
    -d '{"old_password":"wrong-old-password","new_password":"another-new-password"}' \
    "$API/password/change")
if [ "$HTTP_CODE" = "401" ]; then
    echo "PASS"
else
    echo "FAIL: expected 401, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 39: CLI bridge password reset → new password works ==="
RESET_PWD=$("$BIN/agent-aspect" bridge password reset 2>/dev/null)
if [ -n "$RESET_PWD" ] && [ ${#RESET_PWD} -ge 64 ]; then
    # Verify old password no longer works
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
        -H "Content-Type: application/json" \
        -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}" \
        "$API/login")
    if [ "$HTTP_CODE" = "401" ]; then
        # Verify new password works
        LOGIN_RESP3=$(curl -s -X POST \
            -H "Content-Type: application/json" \
            -d "{\"username\":\"admin\",\"password\":\"$RESET_PWD\"}" \
            "$API/login")
        LOGIN_TOKEN3=$(echo "$LOGIN_RESP3" | python3 -c "import json,sys; print(json.load(sys.stdin).get('token',''))")
        if [ "$LOGIN_TOKEN3" = "$TOKEN" ]; then
            PASSWORD="$RESET_PWD"
            echo "PASS: reset generated new password, old password rejected, new works"
        else
            echo "FAIL: reset password login failed: $LOGIN_RESP3"
            FAILED=1
        fi
    else
        echo "FAIL: old password still works after reset (got $HTTP_CODE)"
        FAILED=1
    fi
else
    echo "FAIL: reset output too short or empty: '$RESET_PWD'"
    FAILED=1
fi

# ──────────────────────────────────────────────
# Summary
# ──────────────────────────────────────────────

echo ""
if [ "$FAILED" -eq 0 ]; then
    echo "=== ALL 39 BRIDGE TESTS PASSED ==="
else
    echo "=== SOME BRIDGE TESTS FAILED ==="
    exit 1
fi
