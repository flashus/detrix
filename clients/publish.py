#!/usr/bin/env python3
"""Automated publishing for Detrix client SDKs.

Two-step workflow:
  1. task clients:bump              — sync VERSION into all manifests, then commit + push
  2. task clients:publish           — verify versions, run checks, publish to registries

Direct usage:
    python3 publish.py --bump-only           # step 1: sync versions only
    python3 publish.py                       # step 2: verify + checks + publish all
    python3 publish.py --dry-run             # preview step 2 without publishing
    python3 publish.py --only python         # publish one client
    python3 publish.py --skip-checks         # skip pre-publish checks
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

CLIENTS_DIR = Path(__file__).resolve().parent
VERSION_FILE = CLIENTS_DIR / "VERSION"

SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+(-[\w.]+)?$")

# ── Colour helpers ──────────────────────────────────────────────────────────

GREEN = "\033[32m"
YELLOW = "\033[33m"
RED = "\033[31m"
BOLD = "\033[1m"
RESET = "\033[0m"


def info(msg: str) -> None:
    print(f"{GREEN}✓{RESET} {msg}")


def warn(msg: str) -> None:
    print(f"{YELLOW}⚠{RESET} {msg}")


def error(msg: str) -> None:
    print(f"{RED}✗{RESET} {msg}", file=sys.stderr)


def header(msg: str) -> None:
    print(f"\n{BOLD}── {msg} ──{RESET}")


# ── Version helpers ─────────────────────────────────────────────────────────

# (path, search pattern, replacement template)
def _version_targets(version: str) -> list[tuple[Path, str, str]]:
    return [
        (
            CLIENTS_DIR / "python" / "pyproject.toml",
            r'(version\s*=\s*")[^"]+(")',
            rf"\g<1>{version}\2",
        ),
        (
            CLIENTS_DIR / "python" / "detrix" / "__init__.py",
            r'(__version__\s*=\s*")[^"]+(")',
            rf"\g<1>{version}\2",
        ),
        (
            CLIENTS_DIR / "rust" / "Cargo.toml",
            r'(version\s*=\s*")[^"]+(")',
            rf"\g<1>{version}\2",
        ),
        (
            CLIENTS_DIR / "go" / "version.go",
            r'(Version\s*=\s*")[^"]+(")',
            rf"\g<1>{version}\2",
        ),
    ]


def read_version() -> str:
    if not VERSION_FILE.exists():
        error(f"Version file not found: {VERSION_FILE}")
        sys.exit(1)
    version = VERSION_FILE.read_text().strip()
    if not SEMVER_RE.match(version):
        error(f"Invalid semver in {VERSION_FILE}: {version!r}")
        sys.exit(1)
    return version


def _replace_first(path: Path, pattern: str, replacement: str) -> bool:
    """Replace the first regex match in a file. Returns True if changed."""
    text = path.read_text()
    new_text, count = re.subn(pattern, replacement, text, count=1)
    if count == 0:
        warn(f"  Pattern not found in {path.relative_to(CLIENTS_DIR)}")
        return False
    if new_text == text:
        return False
    path.write_text(new_text)
    return True


def sync_versions(version: str) -> None:
    """Write *version* into every client's manifest file."""
    header("Syncing versions")
    for path, pattern, replacement in _version_targets(version):
        rel = path.relative_to(CLIENTS_DIR)
        if not path.exists():
            warn(f"  {rel} does not exist, skipping")
            continue
        changed = _replace_first(path, pattern, replacement)
        if changed:
            info(f"  Updated {rel} → {version}")
        else:
            info(f"  {rel} already at {version}")


def verify_versions(version: str) -> None:
    """Check every client manifest is already at *version*. Exit if any is stale."""
    header("Verifying versions")
    stale: list[str] = []
    for path, pattern, replacement in _version_targets(version):
        rel = str(path.relative_to(CLIENTS_DIR))
        if not path.exists():
            warn(f"  {rel}: file not found, skipping")
            continue
        text = path.read_text()
        new_text, count = re.subn(pattern, replacement, text, count=1)
        if count == 0:
            error(f"  {rel}: version pattern not found")
            stale.append(rel)
        elif new_text != text:
            error(f"  {rel}: not bumped to {version} yet")
            stale.append(rel)
        else:
            info(f"  {rel}: ok")
    if stale:
        error(
            "\nRun 'task clients:bump', commit the changes, push to main,"
            " then re-run 'task clients:publish'."
        )
        sys.exit(1)


# ── Pre-publish checks ─────────────────────────────────────────────────────


def run_checks(only: str | None) -> None:
    """Run language-specific checks via Taskfile."""
    header("Running pre-publish checks")

    task_bin = shutil.which("task")
    if task_bin is None:
        warn("'task' binary not found – skipping checks")
        return

    checks: list[tuple[str, str]] = [
        ("python", "python-check"),
        ("go", "go-check"),
        ("rust", "rust-check"),
    ]
    for lang, task_name in checks:
        if only and lang != only:
            continue
        info(f"  Running {task_name}...")
        result = subprocess.run(
            [task_bin, task_name],
            cwd=CLIENTS_DIR,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            error(f"  {task_name} failed:\n{result.stdout}\n{result.stderr}")
            sys.exit(1)
        info(f"  {task_name} passed")


# ── Registry checks ────────────────────────────────────────────────────────


def _http_json(url: str, headers: dict[str, str] | None = None) -> dict | None:
    """Fetch JSON from *url*. Returns None on any network error."""
    req = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read())
    except (urllib.error.URLError, OSError, json.JSONDecodeError, ValueError):
        return None


def pypi_published_version() -> str | None:
    data = _http_json("https://pypi.org/pypi/detrix-py/json")
    if data:
        return data.get("info", {}).get("version")
    return None


def crates_published_version() -> str | None:
    data = _http_json(
        "https://crates.io/api/v1/crates/detrix-rs",
        headers={"User-Agent": "detrix-publish-script/1.0"},
    )
    if data:
        return data.get("crate", {}).get("max_version")
    return None


def go_tag_exists(version: str) -> bool:
    tag = f"clients/go/v{version}"
    result = subprocess.run(
        ["git", "ls-remote", "--tags", "origin", f"refs/tags/{tag}"],
        capture_output=True,
        text=True,
        cwd=CLIENTS_DIR,
    )
    return bool(result.stdout.strip())


# ── Publish commands ────────────────────────────────────────────────────────


def publish_python(version: str, dry_run: bool) -> bool:
    header("Python → PyPI")

    published = pypi_published_version()
    if published == version:
        info(f"  Already published at {version} – skipping")
        return True
    if published:
        info(f"  Current PyPI version: {published}")
    else:
        warn("  Could not check PyPI (network error?) – proceeding")

    if dry_run:
        info(f"  [dry-run] Would publish detrix-py {version} to PyPI")
        return True

    token = os.environ.get("UV_PUBLISH_TOKEN")
    if not token:
        error("  UV_PUBLISH_TOKEN not set – cannot publish to PyPI")
        return False

    py_dir = CLIENTS_DIR / "python"
    dist_dir = py_dir / "dist"

    # Clean old builds
    if dist_dir.exists():
        shutil.rmtree(dist_dir)

    # Build
    info("  Building...")
    result = subprocess.run(
        ["uv", "build"],
        cwd=py_dir,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        error(f"  Build failed:\n{result.stdout}\n{result.stderr}")
        return False

    # Publish
    info("  Publishing...")
    result = subprocess.run(
        ["uv", "publish"],
        cwd=py_dir,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        error(f"  Publish failed:\n{result.stdout}\n{result.stderr}")
        return False

    info(f"  Published detrix-py {version} to PyPI")
    return True


def publish_rust(version: str, dry_run: bool) -> bool:
    header("Rust → crates.io")

    published = crates_published_version()
    if published == version:
        info(f"  Already published at {version} – skipping")
        return True
    if published:
        info(f"  Current crates.io version: {published}")
    else:
        warn("  Could not check crates.io (network error?) – proceeding")

    if dry_run:
        info(f"  [dry-run] Would publish detrix-rs {version} to crates.io")
        return True

    rust_dir = CLIENTS_DIR / "rust"
    info("  Publishing...")
    result = subprocess.run(
        ["cargo", "publish"],
        cwd=rust_dir,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        error(f"  Publish failed:\n{result.stdout}\n{result.stderr}")
        return False

    info(f"  Published detrix-rs {version} to crates.io")
    return True


def publish_go(version: str, dry_run: bool) -> bool:
    header("Go → pkg.go.dev")

    tag = f"clients/go/v{version}"

    if go_tag_exists(version):
        info(f"  Tag {tag} already exists – skipping")
        return True

    if dry_run:
        info(f"  [dry-run] Would create and push tag {tag}")
        return True

    # Create tag
    info(f"  Creating tag {tag}...")
    result = subprocess.run(
        ["git", "tag", tag],
        cwd=CLIENTS_DIR,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        error(f"  git tag failed:\n{result.stderr}")
        return False

    # Push tag
    info(f"  Pushing tag {tag}...")
    result = subprocess.run(
        ["git", "push", "origin", tag],
        cwd=CLIENTS_DIR,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        error(f"  git push failed:\n{result.stderr}")
        return False

    info(f"  Published Go module v{version} (tag: {tag})")
    return True


# ── Main ────────────────────────────────────────────────────────────────────

PUBLISHERS = {
    "python": publish_python,
    "rust": publish_rust,
    "go": publish_go,
}


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Publish Detrix client SDKs to package registries.",
    )
    parser.add_argument(
        "--bump-only",
        action="store_true",
        help="Sync VERSION into all client manifests and exit (commit the result before publishing)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Preview publish actions without touching any registry",
    )
    parser.add_argument(
        "--skip-checks",
        action="store_true",
        help="Skip pre-publish checks",
    )
    parser.add_argument(
        "--only",
        choices=["python", "go", "rust"],
        help="Publish only one client",
    )
    args = parser.parse_args()

    version = read_version()
    header(f"Detrix client SDK — v{version}")

    # ── Step 1: bump only ───────────────────────────────────────────────────
    if args.bump_only:
        sync_versions(version)
        print(
            f"\n{BOLD}Next:{RESET} commit the changes above, push to main,"
            f" then run:  task clients:publish\n"
        )
        return

    # ── Step 2: publish ─────────────────────────────────────────────────────
    if args.dry_run:
        warn("Dry-run mode: no changes will be made to registries")

    # 1. Verify all manifests are already at the right version
    verify_versions(version)

    # 2. Pre-publish checks
    if not args.skip_checks:
        run_checks(only=args.only)
    else:
        warn("Skipping pre-publish checks (--skip-checks)")

    # 3. Publish
    results: dict[str, str] = {}
    for lang, publish_fn in PUBLISHERS.items():
        if args.only and lang != args.only:
            continue
        ok = publish_fn(version, dry_run=args.dry_run)
        results[lang] = "ok" if ok else "FAILED"

    # 4. Summary
    header("Summary")
    all_ok = True
    for lang, status in results.items():
        icon = f"{GREEN}✓{RESET}" if status == "ok" else f"{RED}✗{RESET}"
        print(f"  {icon} {lang}: {status}")
        if status != "ok":
            all_ok = False

    if not all_ok:
        sys.exit(1)


if __name__ == "__main__":
    main()
