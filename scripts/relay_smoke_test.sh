#!/usr/bin/env bash
# relay_smoke_test.sh — Relay 冒烟测试
# 验证 relay HTTP 端点：register, proxy routing, UI, auth
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$ROOT/target/debug"

echo "=== building ==="
cargo build --manifest-path "$ROOT/Cargo.toml" --bin agent-aspect-relay --bin agent-aspect-bridge 2>&1

# 隔离 HOME
HOME_OVERRIDE="/tmp/agent-aspect-relay-smoke-$$"
mkdir -p "$HOME_OVERRIDE/.agent-aspect" "$HOME_OVERRIDE/.agent-aspect-relay"
export HOME="$HOME_OVERRIDE"

# relay 环境变量
SETUP_TOKEN="smoke-test-setup-token-$(date +%s)"
export RELAY_SETUP_TOKEN="$SETUP_TOKEN"
RELAY_PORT=$((8800 + RANDOM % 1000))
export RELAY_LISTEN_ADDR="127.0.0.1:$RELAY_PORT"

# bridge 环境变量
export AGENT_ASPECT_RELAY_SETUP_TOKEN="$SETUP_TOKEN"
export AGENT_ASPECT_RELAY_URL="ws://127.0.0.1:$RELAY_PORT/ws"
BRIDGE_PORT=$((7600 + RANDOM % 1000))
export AGENT_ASPECT_BRIDGE_ADDR="127.0.0.1:$BRIDGE_PORT"

# 创建 project dir (bridge 需要)
PROJECT_DIR="$HOME_OVERRIDE/project"
mkdir -p "$PROJECT_DIR"
cd "$PROJECT_DIR"
git init --quiet 2>/dev/null || true
echo "hello" > README.md

echo "=== starting relay on $RELAY_LISTEN_ADDR ==="
"$BIN/agent-aspect-relay" &
RELAY_PID=$!
sleep 1

if ! kill -0 "$RELAY_PID" 2>/dev/null; then
    echo "FAIL: relay did not start"
    exit 1
fi

RELAY_API="http://127.0.0.1:$RELAY_PORT"
FAILED=0

# ============================================================
# Phase 1: Relay-only tests (no bridge needed)
# ============================================================

echo ""
echo "=== test 1: GET / (serves mobile UI) ==="
UI_RESP=$(curl -s "$RELAY_API/")
if grep -q "Agent Aspect" <<< "$UI_RESP"; then
    echo "PASS"
else
    echo "FAIL: UI did not serve"
    FAILED=1
fi

echo ""
echo "=== test 2: GET /api/health without token → 401 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$RELAY_API/api/health")
if [ "$HTTP_CODE" = "401" ]; then
    echo "PASS"
else
    echo "FAIL: expected 401, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 3: GET /api/health with wrong token → 401 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
    -H "Authorization: Bearer wrongtoken" \
    "$RELAY_API/api/health")
if [ "$HTTP_CODE" = "401" ]; then
    echo "PASS"
else
    echo "FAIL: expected 401, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 4: POST /api/register with wrong setup_token → 401 ==="
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Content-Type: application/json" \
    -d '{"setup_token":"wrong-token","label":"smoke"}' \
    "$RELAY_API/api/register")
if [ "$HTTP_CODE" = "401" ]; then
    echo "PASS"
else
    echo "FAIL: expected 401, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 5: POST /api/register → returns tokens ==="
REG_RESP=$(curl -s -X POST \
    -H "Content-Type: application/json" \
    -d "{\"setup_token\":\"$SETUP_TOKEN\",\"label\":\"smoke-test\",\"ttl_days\":1}" \
    "$RELAY_API/api/register")
MAC_TOKEN=$(echo "$REG_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('mac_token',''))" 2>/dev/null)
CLIENT_TOKEN=$(echo "$REG_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('client_token',''))" 2>/dev/null)
SID=$(echo "$REG_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('sid',''))" 2>/dev/null)
if [ -n "$MAC_TOKEN" ] && [ -n "$CLIENT_TOKEN" ] && [ -n "$SID" ]; then
    echo "PASS: sid=$SID"
else
    echo "FAIL: missing tokens in response: $REG_RESP"
    FAILED=1
fi

echo ""
echo "=== test 6: POST /api/register with same setup_token again → new sid ==="
REG_RESP2=$(curl -s -X POST \
    -H "Content-Type: application/json" \
    -d "{\"setup_token\":\"$SETUP_TOKEN\",\"label\":\"smoke-test-2\",\"ttl_days\":1}" \
    "$RELAY_API/api/register")
SID2=$(echo "$REG_RESP2" | python3 -c "import json,sys; print(json.load(sys.stdin).get('sid',''))" 2>/dev/null)
CLIENT_TOKEN2=$(echo "$REG_RESP2" | python3 -c "import json,sys; print(json.load(sys.stdin).get('client_token',''))" 2>/dev/null)
if [ -n "$SID2" ] && [ "$SID2" != "$SID" ]; then
    echo "PASS: new sid=$SID2"
else
    echo "FAIL: expected different sid, got $SID2 (original: $SID)"
    FAILED=1
fi

echo ""
echo "=== test 7: GET /api/overview with client token but no mac connected → 503 ==="
if [ -n "$CLIENT_TOKEN" ]; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -H "Authorization: Bearer $CLIENT_TOKEN" \
        "$RELAY_API/api/overview")
    if [ "$HTTP_CODE" = "503" ]; then
        echo "PASS"
    else
        echo "FAIL: expected 503, got $HTTP_CODE"
        FAILED=1
    fi
fi

echo ""
echo "=== test 8: GET /api/overview with mac_token (wrong role) → 403 ==="
if [ -n "$MAC_TOKEN" ]; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -H "Authorization: Bearer $MAC_TOKEN" \
        "$RELAY_API/api/overview")
    if [ "$HTTP_CODE" = "403" ]; then
        echo "PASS"
    else
        echo "FAIL: expected 403, got $HTTP_CODE"
        FAILED=1
    fi
fi

echo ""
echo "=== test 9: POST /api/session/renew rotates client token ==="
if [ -n "$CLIENT_TOKEN" ]; then
    OLD_CLIENT_TOKEN="$CLIENT_TOKEN"
    RENEW_RESP=$(curl -s -X POST \
        -H "Authorization: Bearer $CLIENT_TOKEN" -H "Content-Type: application/json" \
        -H "X-Device-Id: smoke-mobile" \
        -d '{}' \
        "$RELAY_API/api/session/renew")
    NEW_CLIENT_TOKEN=$(echo "$RENEW_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('client_token',''))" 2>/dev/null)
    if [ -n "$NEW_CLIENT_TOKEN" ] && [ "$NEW_CLIENT_TOKEN" != "$OLD_CLIENT_TOKEN" ]; then
        OLD_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
            -H "Authorization: Bearer $OLD_CLIENT_TOKEN" \
            "$RELAY_API/api/overview")
        NEW_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
            -H "Authorization: Bearer $NEW_CLIENT_TOKEN" \
            "$RELAY_API/api/overview")
        if [ "$OLD_CODE" = "401" ] && [ "$NEW_CODE" = "503" ]; then
            CLIENT_TOKEN="$NEW_CLIENT_TOKEN"
            echo "PASS: old token revoked, new token accepted"
        else
            echo "FAIL: expected old=401 new=503, got old=$OLD_CODE new=$NEW_CODE"
            FAILED=1
        fi
    else
        echo "FAIL: renew response invalid: $RENEW_RESP"
        FAILED=1
    fi
fi

echo ""
echo "=== test 10: GET /api/pending (not in allowlist, but added) → 503 (no mac) ==="
if [ -n "$CLIENT_TOKEN" ]; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -H "Authorization: Bearer $CLIENT_TOKEN" \
        "$RELAY_API/api/pending")
    if [ "$HTTP_CODE" = "503" ]; then
        echo "PASS: proxied but mac offline"
    elif [ "$HTTP_CODE" = "403" ]; then
        echo "FAIL: /pending not in proxy allowlist"
        FAILED=1
    else
        echo "FAIL: expected 503, got $HTTP_CODE"
        FAILED=1
    fi
fi

echo ""
echo "=== test 11: GET /api/run/context (proxied) → 503 (no mac) ==="
if [ -n "$CLIENT_TOKEN" ]; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -H "Authorization: Bearer $CLIENT_TOKEN" \
        "$RELAY_API/api/run/context")
    if [ "$HTTP_CODE" = "503" ]; then
        echo "PASS: proxied but mac offline"
    elif [ "$HTTP_CODE" = "403" ]; then
        echo "FAIL: /run/context not in proxy allowlist"
        FAILED=1
    else
        echo "FAIL: expected 503, got $HTTP_CODE"
        FAILED=1
    fi
fi

echo ""
echo "=== test 12: GET /api/jobs (proxied) → 503 (no mac) ==="
if [ -n "$CLIENT_TOKEN" ]; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -H "Authorization: Bearer $CLIENT_TOKEN" \
        "$RELAY_API/api/jobs")
    if [ "$HTTP_CODE" = "503" ]; then
        echo "PASS: proxied but mac offline"
    elif [ "$HTTP_CODE" = "403" ]; then
        echo "FAIL: /jobs not in proxy allowlist"
        FAILED=1
    else
        echo "FAIL: expected 503, got $HTTP_CODE"
        FAILED=1
    fi
fi

echo ""
echo "=== test 13: GET /api/mobile/summary (proxied) → 503 (no mac) ==="
if [ -n "$CLIENT_TOKEN" ]; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -H "Authorization: Bearer $CLIENT_TOKEN" \
        "$RELAY_API/api/mobile/summary")
    if [ "$HTTP_CODE" = "503" ]; then
        echo "PASS"
    else
        echo "FAIL: expected 503, got $HTTP_CODE"
        FAILED=1
    fi
fi

echo ""
echo "=== test 14: POST /api/decide (proxied) → 503 (no mac) ==="
if [ -n "$CLIENT_TOKEN" ]; then
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
        -H "Authorization: Bearer $CLIENT_TOKEN" -H "Content-Type: application/json" \
        -d '{"event_id":"test","action":"allow"}' \
        "$RELAY_API/api/decide")
    if [ "$HTTP_CODE" = "503" ]; then
        echo "PASS: proxied but mac offline"
    elif [ "$HTTP_CODE" = "403" ]; then
        echo "FAIL: /decide not in proxy allowlist"
        FAILED=1
    else
        echo "FAIL: expected 503, got $HTTP_CODE"
        FAILED=1
    fi
fi

# ============================================================
# Phase 2: Full proxy tests (bridge + relay)
# ============================================================

echo ""
echo "=== starting bridge on $AGENT_ASPECT_BRIDGE_ADDR ==="
"$BIN/agent-aspect-bridge" &
BRIDGE_PID=$!
sleep 2

if ! kill -0 "$BRIDGE_PID" 2>/dev/null; then
    echo "WARN: bridge did not start — skipping proxy tests"
else
    BRIDGE_CLIENT_TOKEN_PATH="$HOME_OVERRIDE/.agent-aspect/relay.client_token"
    BRIDGE_CLIENT_TOKEN=""
    for i in $(seq 1 15); do
        if [ -s "$BRIDGE_CLIENT_TOKEN_PATH" ]; then
            BRIDGE_CLIENT_TOKEN="$(cat "$BRIDGE_CLIENT_TOKEN_PATH")"
            break
        fi
        sleep 1
    done

    if [ -z "$BRIDGE_CLIENT_TOKEN" ]; then
        echo "FAIL: bridge did not write relay client token"
        FAILED=1
    fi

    # Wait for bridge to connect to relay via WebSocket
    echo "=== waiting for bridge relay connection ==="
    CONNECTED=0
    for i in $(seq 1 20); do
        if [ -z "$BRIDGE_CLIENT_TOKEN" ]; then
            break
        fi
        # Try a health check — if bridge is connected, it should work
        HC=$(curl -s -o /dev/null -w "%{http_code}" \
            -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" \
            "$RELAY_API/api/health" 2>/dev/null || echo "000")
        if [ "$HC" = "200" ]; then
            CONNECTED=1
            break
        fi
        sleep 1
    done

    if [ "$CONNECTED" -eq 1 ]; then
        echo "PASS: bridge connected via relay"

        echo ""
        echo "=== test 15: GET /api/health through relay → 200 ==="
        HEALTH=$(curl -s -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" "$RELAY_API/api/health")
        if echo "$HEALTH" | grep -q '"status":"ok"'; then
            echo "PASS"
        else
            echo "FAIL: $HEALTH"
            FAILED=1
        fi

        echo ""
        echo "=== test 16: GET /api/overview through relay → 200 ==="
        OVERVIEW=$(curl -s -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" "$RELAY_API/api/overview?limit=3")
        HAS_CONVERSATIONS=$(echo "$OVERVIEW" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if 'conversations' in d else 'no')" 2>/dev/null || echo "no")
        if [ "$HAS_CONVERSATIONS" = "yes" ]; then
            echo "PASS"
        else
            echo "FAIL: response missing conversations: $OVERVIEW"
            FAILED=1
        fi

        echo ""
        echo "=== test 17: GET /api/pending through relay → 200 ==="
        PENDING=$(curl -s -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" "$RELAY_API/api/pending")
        HAS_EVENTS=$(echo "$PENDING" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if 'events' in d else 'no')" 2>/dev/null || echo "no")
        if [ "$HAS_EVENTS" = "yes" ]; then
            echo "PASS"
        else
            echo "FAIL: response missing events: $PENDING"
            FAILED=1
        fi

        echo ""
        echo "=== test 18: GET /api/run/context through relay → 200 ==="
        CTX=$(curl -s -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" "$RELAY_API/api/run/context")
        HAS_PROJECTS=$(echo "$CTX" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if 'projects' in d else 'no')" 2>/dev/null || echo "no")
        if [ "$HAS_PROJECTS" = "yes" ]; then
            echo "PASS"
        else
            echo "FAIL: response missing projects: $CTX"
            FAILED=1
        fi

        echo ""
        echo "=== test 19: GET /api/jobs through relay → 200 ==="
        JOBS=$(curl -s -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" "$RELAY_API/api/jobs")
        HAS_JOBS=$(echo "$JOBS" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if 'jobs' in d else 'no')" 2>/dev/null || echo "no")
        if [ "$HAS_JOBS" = "yes" ]; then
            echo "PASS"
        else
            echo "FAIL: response missing jobs: $JOBS"
            FAILED=1
        fi

        echo ""
        echo "=== test 20: GET /api/mobile/summary through relay → 200 ==="
        SUMMARY=$(curl -s -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" "$RELAY_API/api/mobile/summary")
        HAS_SUMMARY=$(echo "$SUMMARY" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if 'overview' in d and 'pending' in d and 'last_job' in d else 'no')" 2>/dev/null || echo "no")
        if [ "$HAS_SUMMARY" = "yes" ]; then
            echo "PASS"
        else
            echo "FAIL: missing mobile summary fields: $SUMMARY"
            FAILED=1
        fi

        echo ""
        echo "=== test 21: POST /api/jobs through relay → 200 ==="
        JOB_RESP=$(curl -s -X POST \
            -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" -H "Content-Type: application/json" \
            -d '{"kind":"git_status"}' \
            "$RELAY_API/api/jobs")
        JOB_ID=$(echo "$JOB_RESP" | python3 -c "import json,sys; print(json.load(sys.stdin).get('job_id',''))" 2>/dev/null)
        if [ -n "$JOB_ID" ]; then
            echo "PASS: job_id=$JOB_ID"
        else
            echo "FAIL: no job_id in response: $JOB_RESP"
            FAILED=1
        fi

        echo ""
        echo "=== test 22: POST /api/beat-from-mobile through relay → 200 ==="
        BEAT_RESP=$(curl -s -X POST \
            -H "Authorization: Bearer $BRIDGE_CLIENT_TOKEN" -H "Content-Type: application/json" \
            -H "X-Device-Id: smoke-mobile" \
            -d '{"request_id":"smoke-beat","device_id":"smoke-mobile","client_sent_at_ms":123}' \
            "$RELAY_API/api/beat-from-mobile")
        BEAT_OK=$(echo "$BEAT_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if d.get('type') == 'beat_ack' and d.get('status') == 'ok' else 'no')" 2>/dev/null || echo "no")
        if [ "$BEAT_OK" = "yes" ]; then
            echo "PASS"
        else
            echo "FAIL: beat response invalid: $BEAT_RESP"
            FAILED=1
        fi
    else
        echo "FAIL: bridge did not connect to relay within 20s"
        FAILED=1
    fi

    kill "$BRIDGE_PID" 2>/dev/null || true
    wait "$BRIDGE_PID" 2>/dev/null || true
fi

# ============================================================
# Phase 3: Security boundary tests (after bridge cleanup)
# ============================================================

echo ""
echo "=== test 23: POST oversized body (>1 MiB) → rejected ==="
OVERSIZED_BODY=$(python3 -c "print('x' * (1024*1024+1))")
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Authorization: Bearer $CLIENT_TOKEN" -H "Content-Type: application/json" \
    -d "{\"event_id\":\"$OVERSIZED_BODY\"}" \
    "$RELAY_API/api/decide" 2>/dev/null || echo "000")
if [ "$HTTP_CODE" = "413" ] || [ "$HTTP_CODE" = "000" ]; then
    echo "PASS: oversized body rejected ($HTTP_CODE)"
else
    echo "FAIL: expected 413 or connection drop, got $HTTP_CODE"
    FAILED=1
fi

echo ""
echo "=== test 24: Registration rate limit (>10 in 60s) → 429 ==="
RATE_HIT=0
for i in $(seq 1 12); do
    HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
        -H "Content-Type: application/json" \
        -d "{\"setup_token\":\"rate-test-$i\",\"label\":\"rate-test\",\"ttl_days\":1}" \
        "$RELAY_API/api/register" 2>/dev/null || echo "000")
    if [ "$HTTP_CODE" = "429" ]; then
        RATE_HIT=1
        break
    fi
done
if [ "$RATE_HIT" -eq 1 ]; then
    echo "PASS: rate limit triggered"
else
    echo "FAIL: rate limit not hit within 12 attempts"
    FAILED=1
fi

# Cleanup
kill "$RELAY_PID" 2>/dev/null || true
wait "$RELAY_PID" 2>/dev/null || true

echo ""
if [ "$FAILED" -eq 0 ]; then
    echo "=== ALL RELAY TESTS PASSED ==="
else
    echo "=== SOME RELAY TESTS FAILED ==="
    exit 1
fi
