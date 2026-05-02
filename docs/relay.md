# Relay Deployment Guide

## What Relay Is

Agent Aspect Relay is an optional server that lets your phone reach the Mac bridge when the phone and Mac are on different networks. The bridge connects to the relay over WebSocket; your phone browser connects to the relay over HTTPS. The relay proxies API requests between them.

## What Relay Is Not

- **Not a default dependency.** Agent Aspect works fully without a relay.
- **Not cloud sync.** The relay does not store transcripts, audit data, or user content. It only persists its own secret, setup token, and registered token roster.
- **Not an account system.** There are no user accounts, passwords, or email sign-ups.
- **Not multi-tenant.** One relay serves one Mac. Run multiple relays for multiple Macs.

## Recommended Deployment Order

| Path | When to use | Setup |
|------|-------------|-------|
| **Local only** | Mac and phone on same LAN | Just use the bridge directly. |
| **LAN / Tailscale** | Mac and phone on same network or Tailscale mesh | Set `bridge_lan_enabled = true` in config. |
| **Self-hosted relay** | Phone on mobile data, Mac elsewhere | Run `agent-aspect-relay` on a VPS you control. |

## Setup

### 1. Run the relay on your VPS

```bash
# Build
cargo install --path crates/relay

# Run
agent-aspect-relay
```

On first start, the relay generates:
- `~/.agent-aspect-relay/relay.secret` -- HMAC signing key
- `~/.agent-aspect-relay/setup.token` -- one-time registration token

The setup token is printed to stderr on first generation. Copy it.

### 2. Configure the relay URL on your Mac

```bash
agent-aspect bridge relay set-url wss://relay.example.com/ws
```

### 3. Provide the setup token

Either place it in a file:

```bash
echo "<setup-token>" > ~/.agent-aspect/relay.setup_token
```

Or set it as an environment variable:

```bash
export AGENT_ASPECT_RELAY_SETUP_TOKEN=<setup-token>
```

### 4. Restart the bridge

```bash
agent-aspect bridge restart
```

The bridge automatically registers with the relay on startup. It generates a `sid` (session ID), obtains mac and client tokens, and connects via WebSocket. The connection reconnects automatically on disconnect.

### 5. Check relay status

```bash
agent-aspect bridge relay status
```

### 6. Get the client token for your phone

```bash
agent-aspect bridge token --relay-client
```

### 7. Access from your phone

Open `https://relay.example.com/` in your phone browser. Enter the client token when prompted.

### LAN access (optional)

If your phone and Mac are on the same network, you can skip the relay and use LAN access directly:

```bash
agent-aspect bridge expose
agent-aspect bridge pair
```

`pair` shows the LAN URL and token hint. `expose` binds the bridge to `0.0.0.0` so other devices can reach it.

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `RELAY_LISTEN_ADDR` | `0.0.0.0:8080` | Address and port the relay listens on |
| `RELAY_LISTEN_PORT` | `8080` | Host port mapped to the container port (Docker only) |
| `RELAY_SECRET` | (auto-generated) | HMAC signing key. Override to use a specific secret. |
| `RELAY_SETUP_TOKEN` | (auto-generated) | Registration token. Override to use a specific token. |

## Docker

A `docker-compose.relay.yml` is provided in the repository root for running the relay in a container:

```bash
cp .env.example .env
# Edit .env with your values
docker compose -f docker-compose.relay.yml up -d
```

The Docker setup only covers `agent-aspect-relay`. The daemon, bridge, hook-cli, and CLI are not containerized -- they run natively on your Mac.

## TLS

If you expose the relay on the public internet, you must put it behind a TLS-terminating reverse proxy (nginx, Caddy, etc.). The relay itself does not handle TLS.

```
Phone ── HTTPS ──> Caddy/nginx ── HTTP ──> agent-aspect-relay
```

Example Caddyfile:

```
relay.example.com {
    reverse_proxy localhost:8080
}
```

## Disconnecting

To disconnect from the relay:

```bash
agent-aspect bridge relay unset-url
agent-aspect bridge restart
```

This removes the relay URL from config and restarts the bridge without a relay connection. The registration on the relay side remains until the tokens expire (default 30 days) or the relay operator removes them.

## Security Notes

- The relay does not log request bodies, tokens, or prompts.
- POST bodies are limited to 1 MiB.
- Each session allows at most 100 concurrent pending requests.
- Registration is rate-limited to 10 attempts per 60 seconds.
- If you do not trust a relay, do not use it.
- See [security.md](security.md) for the full security model.
