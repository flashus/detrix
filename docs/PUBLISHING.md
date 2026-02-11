# Publishing Detrix Client SDKs

Instructions for publishing client libraries to their respective package registries. This is a maintainer-only document.

## Python (PyPI)

Package name: `detrix-py`

```bash
cd clients/python

# Build
uv build
# or: python -m build

# Publish (requires PyPI credentials)
uv publish
# or: twine upload dist/*
```

Verify metadata in `pyproject.toml` before publishing. The `readme`, `license`, `classifiers`, and `urls` fields are all used by PyPI to render the package page.

## Go (pkg.go.dev)

Module path: `github.com/flashus/detrix/clients/go`

Go modules are published by pushing a git tag. pkg.go.dev auto-indexes from GitHub:

```bash
git tag clients/go/v1.0.0
git push origin clients/go/v1.0.0
```

No build step or registry upload needed. After pushing the tag, pkg.go.dev will index the module within a few hours (or trigger it manually by visiting the module URL).

## Rust (crates.io)

Crate name: `detrix-rs`

```bash
cd clients/rust

# Dry-run to verify packaging
cargo publish --dry-run

# Publish (requires crates.io token via `cargo login`)
cargo publish
```

Documentation is auto-published to [docs.rs/detrix-rs](https://docs.rs/detrix-rs) after the crate is published.

Verify `Cargo.toml` has: `readme`, `documentation`, `repository`, `license`, `description`, `keywords`, and `categories`.

## Pre-publish Checklist

- [ ] Version numbers updated in all three clients
- [ ] CHANGELOG updated
- [ ] All tests pass (`cargo test`, `uv run pytest`, `go test ./...`)
- [ ] LICENSE file present in each client directory
- [ ] README renders correctly (check on GitHub)
- [ ] No secrets or dev-only files included in package
