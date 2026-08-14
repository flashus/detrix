# Rust eBPF agent mode

Rust eBPF observation is an explicit opt-in backend. Set
`DETRIX_AGENT_RUST_EBPF=1` in the privileged Linux test harness or request
`capture_backend=ebpf` for a Rust connection. `auto` continues to select DAP
for Rust until the release gates are complete.

## Supported pilot

The current live profile supports scalar integers/floats/bools, pointer and
reference addresses, bounded inline structs/fixed arrays, owned `String`,
borrowed `&str`, `Vec<T>`, and borrowed slices. The latter headers are bounded:
the probe captures pointer/length metadata and the userspace reader applies
the configured byte/element limits. Rust `Vec<T>` follows the codelldb/rustc
`RawVec` distinction; borrowed slices are two-word fat pointers and expose a
bounded `cap == len` compatibility value because they have no capacity word.

Rust header locations use the language-neutral `StringHeader` and
`SliceHeader` IR variants. Go retains its compatibility `GoString` and
`GoSlice` variants; both lower to the same bounded wire envelope, but Rust no
longer depends on Go runtime terminology at the DWARF/profile boundary.

Values must have usable DWARF locations at the selected source-line PC.
Optimized-out or unavailable values fail closed. Explicit, non-niche enums
with compiler-emitted discriminants and bounded payload fields are supported
by the current pilot; niche `Option<T>`, trait objects, and async-generator
state remain disabled capabilities until each has an explicit IR
representation and independent fixture plus privileged evidence gates pass.

## codelldb comparison and representation boundary

The Rust formatter shipped with codelldb/rustc (`lldb_providers.py`) is a
useful layout reference, not a capture backend. It unwraps `String` through
`Vec -> RawVec -> NonNull/Unique -> pointer`, reads `len`, and then performs a
process-memory read. Its `Vec<T>` provider explicitly handles the RawVec
layout variants used by Rust 1.75 and 1.76; its slice provider treats `&[T]`
as the two-word `{data_ptr, length}` fat pointer. Detrix mirrors these bounded
header offsets in the Rust profile and snapshots only pointer/length metadata
in eBPF, leaving heap bytes to the userspace reader.

codelldb also selects regular enum variants from compiler-provided synthetic
children and has target-specific providers for encoded/niche forms. That is
safe for an interactive debugger which can inspect arbitrary target memory;
it is not sufficient evidence for a generic asynchronous eBPF event decoder.
Detrix therefore accepts an enum only when DWARF exposes a real discriminant,
explicit values, and bounded payload ranges. Rustc may put a variant label on
the payload member rather than the `DW_TAG_variant` DIE; the parser accepts
that representation while retaining the same bounded-discriminant requirement.
Niche layouts remain fail-closed:
we do not infer `Option<T>`'s invalid-pointer encoding from a type name or a
generic byte size. A future niche profile must provide compiler/version/target
metadata plus live fixtures before enabling the capability.

Rust fixtures should use:

```toml
[profile.release]
debug = 2
strip = "none"
opt-level = 0
```

The initial diagnostic build also uses:

```text
RUSTFLAGS="-C force-frame-pointers=yes"
```

The optimized-out control is intentionally separate: it uses the same debug
level and frame-pointer setting but overrides the binary with
`-C opt-level=3`, so compiler-elided locals are tested without weakening the
stable locations used by the positive fixtures.

The build task records compiler version, target triple, flags, source revision,
working-tree state, source diff hash, and fixture binary hashes in
`/private/tmp/detrix-session-target/out/rust-ebpf-build-metadata.txt`.

## Runtime requirements

Live tests require Linux, a privileged container, BPF/uProbe permissions,
clang with the BPF target, libbpf/kernel headers, and the shared playground
image. The executable used for attachment and the debug image used for DWARF
may be different; connection diagnostics report `embedded`, `external`,
`split`, or `missing` provenance. Split DWARF is currently fail-closed until
`.dwo`/`.dwp` loading has fixture evidence. The `detrix-ebpf` crate exposes a
reserved `split-dwarf` feature for the future supplementary-section
implementation; it is disabled by default and does not change fail-closed
behavior when enabled.

## Reproducible commands

Run from `detrix-release`:

```bash
task tests:test-agent-rust
task tests:test-agent-rust-composite
task tests:test-agent-rust-reconnect
task tests:test-agent-lifecycle
task tests:test-agent-rust-optimized-unavailable
task tests:test-agent-rust-external-debug
task tests:test-dap-rust-semantic-control
```

All host and Docker Cargo targets and test artifacts use the single disposable
root `/private/tmp/detrix-session-target`. After three or four build/test
cycles, clean only that root with:

```bash
task tests:clean-session-target
```

Named Docker dependency volumes and downloaded base images are preserved.

`tests:test-agent-rust-optimized-unavailable` builds a second fixture with
`-C opt-level=3` and verifies that a dead local is rejected before probe
installation. `tests:test-agent-rust-external-debug` pairs a stripped
executable with a GNU debuglink image; its provider gate is runnable on the
host, while the process-level gate still requires privileged Docker access.

## Evidence rules

Treat a green structural test as insufficient by itself. A live claim should
retain the exact command, source revision/diff hash, compiler/target/flags,
host architecture, binary hashes, persisted metric values, connection
backend/profile diagnostics, and kernel/transport/decode/unavailable counters.
Latency alone is not a bug report; value/location, lifecycle, or event-accounting
evidence is required.
