# Detrix Installation Guide

---

## 1. Install

### Detrix

**macOS** (Homebrew):
```bash
brew install flashus/tap/detrix
```

**macOS / Linux** (shell script):
```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/flashus/detrix/releases/latest/download/detrix-installer.sh | sh
```

**Windows** (PowerShell):
```powershell
irm https://github.com/flashus/detrix/releases/latest/download/detrix-installer.ps1 | iex
```

**Docker** (linux/amd64, linux/arm64):
```bash
docker pull ghcr.io/flashus/detrix:latest
```

**Build from source:** see [BUILD.md](BUILD.md).

```bash
detrix init   # creates config and sets up local storage
```

### Language debugger

Detrix connects to your language's debugger — install the one you need:

**Python** — [debugpy](https://github.com/microsoft/debugpy):
```bash
pip install debugpy
```

**Go** — [Delve](https://github.com/go-delve/delve):
```bash
go install github.com/go-delve/delve/cmd/dlv@latest
```

**Rust** — [lldb-dap](https://lldb.llvm.org/) (part of LLVM):
```bash
# macOS
brew install llvm

# Linux
apt install lldb

# Windows
scoop install llvm
```

---

## 2. Start your app with the debugger

Listens on `127.0.0.1` — local only. See [Cloud Debugging](#cloud-debugging) for remote and Docker.

**Python:**
```bash
python -m debugpy --listen 127.0.0.1:5678 app.py
```

**Go:**
```bash
dlv debug --headless --listen=127.0.0.1:5678 --api-version=2 main.go
```

**Rust:**
```bash
lldb-dap --port 5678
```

> **Prefer zero config?** Embed `detrix.init()` once and skip this step entirely — the agent wakes the debugger on demand. See [App Integration](../README.md#app-integration).

---

## 3. AI Agent Setup

### Claude Code

```bash
claude mcp add --scope user detrix -- detrix mcp
```

Restart Claude Code, then verify with `/mcp` — should show `detrix`.

### Cursor

Add to your Cursor settings (`~/Library/Application Support/Cursor/User/settings.json` on macOS, `%APPDATA%\Cursor\User\settings.json` on Windows):

```json
{
  "mcp.servers": {
    "detrix": {
      "command": "detrix",
      "args": ["mcp"]
    }
  }
}
```

Restart Cursor.

### Windsurf

Add to your Windsurf settings (`~/Library/Application Support/Windsurf/User/settings.json` on macOS, `%APPDATA%\Windsurf\User\settings.json` on Windows):

```json
{
  "mcp.servers": {
    "detrix": {
      "command": "detrix",
      "args": ["mcp"]
    }
  }
}
```

Restart Windsurf.

---

## Cloud Debugging

Observe code running inside Docker containers or remote hosts. The AI agent connects to a Detrix daemon deployed alongside your service — no VPN, no port forwarding needed.

### How it works

1. **Detrix daemon** runs in Docker alongside your service (set `DETRIX_ADVERTISE_URL` so it knows its public address)
2. **Detrix client** embedded in your app registers with the daemon on startup and exposes a `/detrix/discover` endpoint
3. **AI agent** calls `wake` on your app → the app returns its daemon URL → the bridge auto-switches to that cloud daemon and fetches source files automatically
4. Register multiple cloud daemons in `~/detrix/daemons.toml`; the agent can switch between them by alias or automatically via `wake`

### Setup

**1. Add Detrix daemon to your `docker-compose.yml`:**

```yaml
services:
  detrix:
    image: ghcr.io/flashus/detrix:latest
    ports:
      - "8090:8090"
    environment:
      DETRIX_TOKEN: your-secret-token
      DETRIX_ADVERTISE_URL: http://your-host:8090
    volumes:
      - detrix-data:/data/detrix
      - ./detrix.toml:/data/detrix/detrix.toml:ro

  your-service:
    # ... your service config
    environment:
      DETRIX_CLIENT_ENABLED: "1"
      DETRIX_DAEMON_URL: http://detrix:8090
      DETRIX_TOKEN: your-secret-token
      DETRIX_WORKSPACE_ROOT: /path/to/source/in/container
```

**2. Add `detrix.toml` for the daemon:**

```toml
[api.rest]
host = "0.0.0.0"
port = 8090

[storage]
path = "/data/detrix/data.db"

[vfs]
source_priority = ["control_plane", "disk"]
fetch_timeout_seconds = 10
```

**3. Embed the Detrix client in your app:**

```python
# Python
import detrix
detrix.init(name="my-service")
```

```go
// Go
import detrix "github.com/flashus/detrix/clients/go"

detrix.Init(detrix.Config{
    Name:      "my-service",
    DaemonURL: os.Getenv("DETRIX_DAEMON_URL"),
    Token:     os.Getenv("DETRIX_TOKEN"),
})
```

```rust
// Rust (Cargo.toml: detrix-rs = "1.2.0")
detrix_rs::init(detrix_rs::Config {
    name: "my-service".into(),
    daemon_url: std::env::var("DETRIX_DAEMON_URL").unwrap_or_default(),
    token: std::env::var("DETRIX_TOKEN").ok(),
    ..Default::default()
});
```

**4. Register cloud daemons for the AI agent:**

```toml
# ~/detrix/daemons.toml
[[daemon]]
alias = "staging"
url = "http://staging-host:8090"

[[daemon]]
alias = "production"
url = "http://prod-host:8090"
is_production = true   # requires force=true to switch to
```

Auth tokens in `~/detrix/credentials.toml` (or set `DETRIX_TOKEN` as a global fallback):

```toml
[targets."staging-host:8090"]
token = "staging-secret"

[targets."prod-host:8090"]
token = "prod-secret"
```

**Switching daemons:** The agent uses the `switch_daemon` MCP tool (by alias or URL). Production daemons require explicit `force=true` to prevent accidental switches.

**Auto-switching via `wake`:** When the agent calls `wake` on an app with the Detrix client embedded, the app returns its daemon URL and the bridge switches automatically — no manual `switch_daemon` call needed.

See `examples/docker-demo/` for a complete working example.

---

## Authentication

Detrix is **secure by default**. When no `[api.auth]` section is configured, the daemon auto-generates a token saved to `~/detrix/auth-token`. The MCP bridge discovers it automatically — no setup needed for single-user local development.

For cloud and multi-user deployments, configure explicit authentication in `detrix.toml`:

```toml
[api.auth]
mode = "simple"

[[api.auth.users]]
token = "dtx_alice_secret"
user_id = "alice"
role = "user"

[[api.auth.users]]
token = "dtx_admin_secret"
user_id = "admin"
role = "admin"
```

Each developer sets their token via `DETRIX_TOKEN`:

```bash
DETRIX_TOKEN=dtx_alice_secret detrix mcp
```

Or per-daemon in `~/detrix/credentials.toml`:

```toml
[targets."your-host:8090"]
token = "dtx_alice_secret"
```

Detrix also supports JWT/JWKS for enterprise SSO. See the full [Authentication Guide](AUTH.md) for details on auth modes, access control, tenant ID validation, and multi-tenant configuration.

---

## Troubleshooting

### MCP server not showing up

1. Verify `detrix` is in PATH: `detrix --version`
2. Restart the AI client completely (quit and reopen)
3. Check logs:
   - Claude Code: run `/mcp` to see server status
   - Cursor: Help → Toggle Developer Tools
   - Windsurf: Help → Toggle Developer Tools

### Can't connect to debugger

```bash
# Verify debugger is listening
lsof -i :5678        # macOS / Linux
netstat -an | findstr 5678   # Windows
```

Make sure the debugger is bound to `127.0.0.1:5678`, not a different port.

### Expression blocked

Detrix validates expressions before capture. Use simple variable access:
- ✅ `user.id`, `order.total`, `len(items)`
- ❌ `eval(code)`, `open('file')`

Configure allowed functions in `detrix.toml` — see [Clients Manual](CLIENTS.md).

---

## Links

- [README](../README.md)
- [Authentication Guide](AUTH.md)
- [CLI Reference](CLI.md)
- [Clients Manual](CLIENTS.md)
- [Build from Source](BUILD.md)
