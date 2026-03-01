# Detrix — Build from Source

For binary installs (Homebrew, shell script, Docker) see [INSTALL.md](INSTALL.md).

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Build Detrix](#build-detrix)
3. [Configure AI Client](#configure-ai-client)
4. [Cloud Debugging](#cloud-debugging)
5. [Verification](#verification)
6. [Troubleshooting](#troubleshooting)

---

## Prerequisites

### Required

- **Rust 1.80+** - [Install Rust](https://rustup.rs/)
- **Git** - For cloning the repository
- **Protocol Buffers Compiler (protoc)** - Required for gRPC compilation

### Platform-Specific Setup

#### macOS

```bash
# Install Rust
brew install rustup

# Install protobuf compiler
brew install protobuf

# Verify
protoc --version
```

#### Linux (Ubuntu/Debian)

```bash
# Install protobuf compiler
sudo apt update
sudo apt install -y protobuf-compiler

# Verify
protoc --version
```

#### Windows

**Using [Scoop](https://scoop.sh/)**

1. **Install Visual Studio Build Tools:**
   - Download from [Visual Studio Downloads](https://visualstudio.microsoft.com/downloads/)
   - Select "Desktop development with C++" workload
   - This provides MSVC compiler and Windows SDK

2. **Install Rust:**
   ```powershell
   # Download and run rustup-init.exe from https://rustup.rs/ or `scoop install rustup`
   rustup-init.exe
   ```

3. **Install Protocol Buffers:**
   ```powershell
   # Install protobuf via Scoop
   scoop bucket add extras
   scoop install protobuf
   ```

### For Monitoring Different Languages

Choose based on what you want to monitor:

- **Python**: Python 3.8+ with `debugpy`
  ```bash
  pip install debugpy
  ```

- **Go**: Go 1.19+ with `delve`
  ```bash
  go install github.com/go-delve/delve/cmd/dlv@latest
  ```

- **Rust**: Rust toolchain with `lldb-dap`
  ```bash
  # macOS
  brew install llvm

  # Linux
  apt install lldb

  # Windows (via LLVM installer or Scoop)
  scoop install llvm
  ```

---

## Build Detrix

### macOS / Linux

```bash
# Clone repository
git clone https://github.com/flashus/detrix.git
cd detrix

# Build release binary
cargo build --release

# Verify build
./target/release/detrix --version

# Initialize configuration (creates ~/detrix/detrix.toml)
./target/release/detrix init
```

Binary location: `./target/release/detrix`

**Get absolute path** (you'll need this for configuration):
```bash
echo "$(pwd)/target/release/detrix"
```

**Configuration discovery:**
- Default location: `~/detrix/detrix.toml`
- Custom location: Use `--config <path>` or set `DETRIX_CONFIG` env var
- Custom init: `./target/release/detrix init --path /custom/path/detrix.toml`

### Windows

```powershell
# Clone repository
git clone https://github.com/flashus/detrix.git
cd detrix

# Build release binary
cargo build --release

# Verify build
.\target\release\detrix.exe --version

# Initialize configuration (creates ~/detrix/detrix.toml)
.\target\release\detrix.exe init
```

Binary location: `.\target\release\detrix.exe`

**Get absolute path** (you'll need this for configuration):
```powershell
Write-Output "$PWD\target\release\detrix.exe"
```

**Configuration discovery:**
- Default location: `~/detrix/detrix.toml` (Windows: `%USERPROFILE%\detrix\detrix.toml`)
- Custom location: Use `--config <path>` or set `DETRIX_CONFIG` env var
- Custom init: `.\target\release\detrix.exe init --path C:\custom\path\detrix.toml`

---

## Configure AI Client

Choose your AI client:

- [Claude Code](#claude-code-skill--mcp)
- [Cursor](#cursor)
- [Windsurf](#windsurf)

---

### Claude Code (Skill + MCP)

**What you're installing:**
- Skill at `~/.claude/skills/detrix/SKILL.md` (optional)
- MCP via project `.mcp.json` file

**Steps:**

1. **Create skill directory:**
   ```bash
   mkdir -p ~/.claude/skills/detrix
   ```

2. **Copy skill file:**
   ```bash
   cp skills/detrix/* ~/.claude/skills/detrix/
   ```

3. **Create `.mcp.json` in your project:**
   ```bash
   cat > .mcp.json <<EOF
   {
     "mcpServers": {
       "detrix": {
         "command": "/absolute/path/to/detrix/target/release/detrix",
         "args": ["mcp"],
         "env": {
           "RUST_LOG": "info"
         }
       }
     }
   }
   EOF
   ```

4. **Enable MCP in settings:**

   Edit `~/.claude/settings.json`:
   ```json
   {
     "enabledMcpjsonServers": ["detrix"]
   }
   ```

5. **Restart Claude Code**

6. **Verify:**
   ```
   /skills    → Should show "detrix"
   /mcp       → Should show "detrix" server
   ```

---

### Cursor

**Configuration file location:**
- macOS: `~/Library/Application Support/Cursor/User/settings.json`
- Linux: `~/.config/Cursor/User/settings.json`
- Windows: `%APPDATA%\Cursor\User\settings.json`

**Steps:**

1. **Edit settings:**
   ```bash
   # macOS
   nano ~/Library/Application\ Support/Cursor/User/settings.json

   # Linux
   nano ~/.config/Cursor/User/settings.json
   ```

   ```powershell
   # Windows
   notepad $env:APPDATA\Cursor\User\settings.json
   ```

2. **Add MCP server:**
   ```json
   {
     "mcp.servers": {
          "detrix": {
            "args": [
               "mcp"
            ],
            "command": "/absolute/path/to/detrix/target/release/detrix",
            "disabled": false,
            "env": {
               "RUST_LOG": "debug"
            }
         }
     }
   }
   ```

3. **Restart Cursor**

---

### Windsurf

**Configuration file location:**
- macOS: `~/Library/Application Support/Windsurf/User/settings.json`
- Linux: `~/.config/Windsurf/User/settings.json`
- Windows: `%APPDATA%\Windsurf\User\settings.json`

**Steps:**

1. **Edit settings:**
   ```bash
   # macOS
   nano ~/Library/Application\ Support/Windsurf/User/settings.json

   # Linux
   nano ~/.config/Windsurf/User/settings.json
   ```

   ```powershell
   # Windows
   notepad $env:APPDATA\Windsurf\User\settings.json
   ```

2. **Add MCP server:**
   ```json
   {
     "mcp.servers": {
          "detrix": {
            "args": [
               "mcp"
            ],
            "command": "/absolute/path/to/detrix/target/release/detrix",
            "disabled": false,
            "env": {
               "RUST_LOG": "debug"
            }
         }
     }
   }
   ```

3. **Restart Windsurf**

---

## Cloud Debugging

Observe code running inside Docker containers or remote hosts. The AI agent connects to a Detrix daemon deployed alongside your service — no VPN, no port forwarding needed.

### How it works

1. **Detrix daemon** runs in Docker alongside your service (set `DETRIX_ADVERTISE_URL` so it knows its public address)
2. **Detrix client** embedded in your app registers with the daemon on startup and exposes a `/detrix/discover` endpoint
3. **AI agent** calls `wake` on your app → the app returns its daemon URL → the bridge auto-switches to that cloud daemon and fetches source files via VFS
4. Register multiple cloud daemons in `~/.detrix/daemons.toml`; the agent can switch between them by alias or automatically via `wake`

### Quick Setup

**1. Add Detrix daemon to your `docker-compose.yml`:**

Build the Detrix server image from source:

```bash
docker build -f fixtures/docker/Dockerfile.server -t detrix-server .
```

Then reference it in your `docker-compose.yml`:

```yaml
services:
  detrix:
    image: detrix-server
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
# Source priority for fetching source files for the AI agent.
# Default: ["control_plane", "disk"]. Add "bridge" to also fetch from the agent's workspace.
source_priority = ["control_plane", "disk"]
fetch_timeout_seconds = 10
```

**3. Embed the Detrix client in your app:**

```go
// Go
import detrix "github.com/flashus/detrix/clients/go"

detrix.Init(detrix.Config{
    Name:      "my-service",
    DaemonURL: os.Getenv("DETRIX_DAEMON_URL"),
    Token:     os.Getenv("DETRIX_TOKEN"),
})
```

```python
# Python
import detrix
detrix.init(name="my-service")
```

```rust
// Rust (Cargo.toml: detrix-rs = "1.1.1")
detrix_rs::init(detrix_rs::Config {
    name: "my-service".into(),
    daemon_url: std::env::var("DETRIX_DAEMON_URL").unwrap_or_default(),
    token: std::env::var("DETRIX_TOKEN").ok(),
    ..Default::default()
});
```

**4. Register cloud daemons for the AI agent:**

The MCP bridge (`detrix mcp`) runs locally and connects to daemons — local or cloud. **No `--daemon-url` flag needed.** Cloud daemons are registered once in `~/detrix/daemons.toml`:

```toml
[[daemon]]
alias = "staging"
url = "http://staging-host:8090"

[[daemon]]
alias = "production"
url = "http://prod-host:8090"
is_production = true   # requires force=true to switch to
```

Auth tokens are stored per-host in `~/detrix/credentials.toml` (or set `DETRIX_TOKEN` as a global fallback):

```toml
[targets."staging-host:8090"]
token = "staging-secret"

[targets."prod-host:8090"]
token = "prod-secret"
```

**Switching daemons:** The agent uses the `switch_daemon` MCP tool (by alias or URL) and `list_known_daemons` to see what's registered. Production daemons require explicit `force=true` to prevent accidental switches.

**Auto-switching via `wake`:** When the agent calls `wake` on an app that has the Detrix client embedded, the app's `/detrix/discover` endpoint returns its daemon URL. The bridge automatically switches to that daemon — no manual `switch_daemon` call needed. This means the agent can work across local and cloud services in the same session just by waking them.

See `examples/docker-demo/` for a complete working example with a Go service.

---

## Optional: Add to PATH

For CLI usage anywhere:

### macOS / Linux

```bash
# Option 1: Copy to user bin
mkdir -p ~/.local/bin
cp target/release/detrix ~/.local/bin/
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc

# Verify
detrix --version
```

### Windows

```powershell
# Copy to user bin folder
mkdir -Force $env:USERPROFILE\.local\bin
Copy-Item target\release\detrix.exe $env:USERPROFILE\.local\bin\

# Add to PATH (run as Administrator or add manually via System Properties)
[Environment]::SetEnvironmentVariable("Path", $env:Path + ";$env:USERPROFILE\.local\bin", "User")

# Verify (restart terminal first)
detrix --version
```

---

## Verification

### Test Detrix Binary

```bash
# Check version
./target/release/detrix --version

# Test MCP server
./target/release/detrix mcp
# Type: {"jsonrpc":"2.0","method":"tools/list","id":1}
# Press Ctrl+C to exit
```

### Test with Debugger

**Python:**

1. Start a Python debugger:
   ```bash
   python -m debugpy --listen 127.0.0.1:5678 --wait-for-client app.py
   ```

2. Use Detrix via AI:
   ```
   "Create a connection to debugpy at 127.0.0.1:5678"
   "Add a metric to observe user.id at auth.py line 42"
   ```

**Go:**

1. Start a Go debugger:
   ```bash
   dlv debug --headless --listen=127.0.0.1:5678 --api-version=2 ./main.go
   ```

2. Use Detrix via AI:
   ```
   "Connect to delve at 127.0.0.1:5678 and observe order.Total at main.go line 55"
   ```

**Rust:**

1. Build with debug symbols and start lldb-dap:
   ```bash
   cargo build
   lldb-dap --port 5678 -- ./target/debug/my-app
   ```

2. Use Detrix via AI:
   ```
   "Connect to lldb-dap at 127.0.0.1:5678 and observe transaction.amount at src/main.rs line 42"
   ```
---

## Troubleshooting

### Build Failures

**Error: `protoc` not found (Windows)**
```powershell
# Verify protoc is installed and in PATH
protoc --version

# If not found, install via Scoop
scoop bucket add extras
scoop install protobuf
```

**Error: linker not found (Windows)**
Install Visual Studio Build Tools with C++ workload


### MCP Not Loading

1. Check binary path is **absolute** (not relative)
2. Verify config file JSON is valid
3. Restart AI client completely (quit and reopen)
4. Check logs:
   - Claude Desktop (macOS): `~/Library/Logs/Claude/`
   - Claude Desktop (Windows): `%APPDATA%\Claude\logs\`
   - Cursor: Developer Tools (Help > Toggle Developer Tools)

### Skill Not Loading (Claude Code)

1. Check skill exists:
   ```bash
   ls ~/.claude/skills/detrix/SKILL.md
   ```

2. Check settings.json:
   ```bash
   cat ~/.claude/settings.json
   # Should contain: "enabledMcpjsonServers": ["detrix"]
   ```

### Connection Issues

**Can't connect to debugger:**
```bash
# macOS / Linux - Verify debugger is running
lsof -i :5678
```

```powershell
# Windows - Verify debugger is running
netstat -an | findstr 5678
   ```

**Expression validation fails:**

Detrix blocks unsafe operations. Use simple expressions:
- ✅ `user.id`, `transaction.amount`, `len(items)`
- ❌ `eval(code)`, `open('file')`

---

## Uninstallation

Remove skill ~/.claude/skills/detrix/
Remove project MCP config (.mcp.json in project directory)
Remove binary /path/to/detrix

For Claude Code/Cursor/Windsurf: edit config file and remove the `detrix` entry.

---

## Links

- [README](../README.md)
- [Architecture](ARCHITECTURE.md)
- [GitHub Issues](https://github.com/flashus/detrix/issues)
