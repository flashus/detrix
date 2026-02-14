# Publishing Detrix Client SDKs

Automated publishing for all three client libraries (Python, Go, Rust) to their respective package registries.

## Quick Start

```bash
# Dry-run — preview what would happen
task c:publish -- --dry-run

# Publish all clients
task c:publish

# Publish a single client
task c:publish -- --only python
```

## Version Management

All client versions are driven from a single source of truth:

```
clients/VERSION    ← edit this file to bump the version
```

When you run `publish.py`, it syncs the version into:

| File | Pattern |
|------|---------|
| `python/pyproject.toml` | `version = "X.Y.Z"` |
| `python/detrix/__init__.py` | `__version__ = "X.Y.Z"` |
| `rust/Cargo.toml` | `version = "X.Y.Z"` |
| `go/version.go` | `const Version = "X.Y.Z"` |

## Registry Details

| Client | Package Name | Registry | Publish Mechanism |
|--------|-------------|----------|-------------------|
| Python | `detrix-py` | [PyPI](https://pypi.org/project/detrix-py/) | `uv build` + `uv publish` |
| Rust | `detrix-rs` | [crates.io](https://crates.io/crates/detrix-rs) | `cargo publish` |
| Go | `github.com/flashus/detrix/clients/go` | [pkg.go.dev](https://pkg.go.dev/github.com/flashus/detrix/clients/go) | git tag `clients/go/vX.Y.Z` |

## CLI Options

```
python3 publish.py [OPTIONS]

Options:
  --dry-run        Preview actions without publishing or modifying files
  --skip-checks    Skip pre-publish checks (task clients:{lang}-check)
  --only LANG      Publish only one client (python, go, rust)
```

## Idempotency

The script checks each registry before publishing:

- **PyPI**: `GET https://pypi.org/pypi/detrix-py/json` — compares `.info.version`
- **crates.io**: `GET https://crates.io/api/v1/crates/detrix-rs` — compares `.crate.max_version`
- **Go**: `git ls-remote --tags origin refs/tags/clients/go/vX.Y.Z`

If the target version is already published, that client is **skipped**. This makes it safe to re-run the command after a partial failure.

## Authentication

| Registry | Credential |
|----------|-----------|
| PyPI | `UV_PUBLISH_TOKEN` environment variable |
| crates.io | `~/.cargo/credentials.toml` (via `cargo login`) |
| Go | Git push access to the origin remote |

## Troubleshooting

**"UV_PUBLISH_TOKEN not set"** — Export your PyPI API token:
```bash
export UV_PUBLISH_TOKEN="pypi-..."
```

**"cargo publish failed"** — Ensure you're logged in:
```bash
cargo login
```

**Go tag already exists locally but not on remote** — Delete and recreate:
```bash
git tag -d clients/go/v1.0.0
git tag clients/go/v1.0.0
git push origin clients/go/v1.0.0
```

**Pre-publish checks fail** — Fix the issue or skip with `--skip-checks`:
```bash
task c:publish -- --skip-checks --only rust
```

**Network errors during registry check** — The script warns but proceeds (assumes not published).

## Build Information Best Practices

All clients support automatic build metadata detection. Use these patterns:

### CI/CD Pipelines

**GitHub Actions:**
```yaml
- name: Build with version info
  env:
    GIT_COMMIT: ${{ github.sha }}
    GIT_TAG: ${{ github.ref_name }}
  run: |
    cd clients/go && go build
    cd clients/rust && cargo build --release
```

**GitLab CI:**
```yaml
build:
  variables:
    GIT_COMMIT: $CI_COMMIT_SHA
    GIT_TAG: $CI_COMMIT_TAG
  script:
    - cd clients/go && go build
    - cd clients/rust && cargo build --release
```

### Docker Builds

Always pass build args:
```bash
docker build \
  --build-arg GIT_COMMIT=$(git rev-parse HEAD) \
  --build-arg GIT_TAG=$(git describe --tags --always) \
  -t myapp:latest .
```

### Local Development

No action needed - clients auto-detect via `git rev-parse HEAD`.
