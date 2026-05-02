# Security Model

## Trust Anchor

**The trust anchor is your Mac.** The daemon, rule engine, audit store, and all policy decisions run locally. The relay is an optional transport layer -- it does not make decisions and does not store user content.

## Components

### Daemon (`agent-aspectd`)

- Runs as a local Unix socket server.
- Evaluates every tool call against the rule engine.
- Stores all events and decisions in a local SQLite database.
- Never sends data to external services.

### Bridge (`agent-aspect-bridge`)

- Token-protected HTTP server on `127.0.0.1:7676` by default.
- All endpoints require Bearer token auth except `GET /health`.
- Token is generated locally via `getrandom` (cryptographically secure) and stored at `~/.agent-aspect/bridge.token`.
- CORS is disabled. No cross-origin requests are accepted.

### Relay (`agent-aspect-relay`)

- Proxies HTTP requests from phone to Mac bridge over WebSocket.
- Does not store audit data, transcripts, or user content.
- Persists only its own secret, setup token, and registered token roster.
- Authenticated via HMAC-signed JWT tokens (mac_token for WebSocket, client_token for HTTP).
- Registration requires a one-time setup_token.
- Does not log request bodies, tokens, or prompts.
- POST bodies are capped at 1 MiB.
- Per-session pending requests are capped at 100.
- Registration is rate-limited (10 attempts per 60 seconds).

## Rules and Enforcement

### Rule Sources

| Source | Description |
|--------|-------------|
| `default` | Built-in rules that ship with Agent Aspect. |
| `learned` | Auto-generated from audit patterns. Requires user acceptance. |
| `user` | Manually created by the user. |

### Enforcement Priority

1. **Explicit deny rules always win.** No learned rule, user rule, or mode setting can override an explicit deny.
2. **Learned rules are a fallback.** They only apply when no static rule matches.
3. **Mode controls the default.** If no rule matches, the mode determines whether the action is allowed or requires confirmation.

### What Learned Rules Cannot Do

- Override an explicit deny rule.
- Auto-allow without the user accepting the suggestion.
- Apply to events that have not been observed in real audit data.

## Device Attribution

Device IDs record which browser or hook made a decision. They are for audit attribution, **not for authentication or authorization**.

| Source | ID derivation |
|--------|---------------|
| Browser | `localStorage` UUID, generated once per browser. |
| Local hook | Fixed device ID from hook config. |
| Fallback | Hash of `remote_addr + User-Agent`. |

## Relay Threat Model

| Threat | Mitigation |
|--------|------------|
| Unauthorized registration | Requires one-time `setup_token`. Rate-limited to 10/minute. |
| Token replay | Tokens are HMAC-signed with expiry. Old tokens are pruned on load. |
| Eavesdropping on public internet | Must use HTTPS/WSS. The relay does not handle TLS itself. |
| Relay operator reads traffic | Relay sees proxied request/response bodies. Do not use an untrusted relay. |
| Brute force token | 64-hex-char setup_token (256 bits). Not feasibly brute-forceable. |

## Permission Inheritance

When a session is started with `bypassPermissions` mode, the bridge injects provider-specific CLI arguments and environment variables to propagate the permission setting to the agent subprocess.

Permission passthrough is **capability-driven**: each provider declares `supports_permission_passthrough`, `permission_mode_cli_arg`, and `permission_mode_env_vars` in its `ProviderConfig`. The bridge reads these fields at command-build time — there are no hardcoded per-provider branches.

Currently verified for Claude Code only:
- CLI: `--dangerously-skip-permissions`
- Env: `VIBE_ISLAND_SKIP=1`

Other providers (Codex CLI, Kimi Code) have `supports_permission_passthrough = false` and silently ignore the stored permission mode.

## What Agent Aspect Does Not Do

- No end-to-end encryption between phone and Mac (relay sees traffic in transit).
- No multi-user authentication or authorization.
- No hosted cloud accounts or SaaS features.
- No persistent user data on the relay (only relay secret, setup token, and registered token roster).

## Recommendations

1. **Local only** is the most secure mode. No network exposure.
2. **Tailscale** gives you encrypted transport without a relay.
3. **Self-hosted relay** is acceptable if you control the VPS and use HTTPS.
4. **Third-party relay** is not recommended. If you must, ensure HTTPS/WSS and accept that the relay operator can read traffic.
