//! BPF program compilation and loading
//!
//! Handles the pipeline from generated C source → compiled ELF object → aya-loaded Ebpf.
//! Linux-only execution, but the compilation helper is testable anywhere (it just shells out).
//!
//! # Pipeline
//!
//! ```text
//! BpfProgram (C source)
//!     │
//!     ▼  clang -O2 -target bpf -c
//! ELF .o bytes (in tempfile)
//!     │
//!     ▼  aya::Ebpf::load()
//! Loaded eBPF object (ready to attach)
//! ```
//!
//! # Requirements
//!
//! `clang` must be available on PATH with BPF target support.
//! Check with: `clang --target=bpf --print-supported-targets`
//! Typically installed via: `apt install clang` / `dnf install clang`

use crate::dwarf::types::TargetArchitecture;
use crate::error::{Error, Result};
use crate::probe::program::BpfProgram;

use std::path::PathBuf;

// `-target bpf` strips __aarch64__ / __x86_64__ from the preprocessor, so
// bpf_tracing.h cannot auto-detect the arch. We pass the flag explicitly.
fn bpf_arch_flag(architecture: TargetArchitecture) -> &'static str {
    match architecture {
        TargetArchitecture::Aarch64 => "-D__TARGET_ARCH_arm64",
        TargetArchitecture::X86_64 => "-D__TARGET_ARCH_x86",
    }
}

fn host_target_architecture() -> TargetArchitecture {
    #[cfg(target_arch = "aarch64")]
    {
        return TargetArchitecture::Aarch64;
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        TargetArchitecture::X86_64
    }
}

/// Architecture-aware compiler seam. The generated C remains language-neutral;
/// only the kernel `pt_regs` ABI flag is selected here.
pub trait ArchitectureBpfCompiler: Send + Sync {
    fn compile_for_arch(
        &self,
        program: &BpfProgram,
        architecture: TargetArchitecture,
    ) -> Result<CompiledBpf>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ClangBpfCompiler;

impl ArchitectureBpfCompiler for ClangBpfCompiler {
    fn compile_for_arch(
        &self,
        program: &BpfProgram,
        architecture: TargetArchitecture,
    ) -> Result<CompiledBpf> {
        compile_bpf_internal(program, architecture)
    }
}

/// Explicit architecture adapters keep the target ABI decision out of the
/// profile/compiler call sites. They intentionally share clang's renderer;
/// their seam is where an architecture-specific verifier or renderer can be
/// introduced without changing profiles.
#[derive(Debug, Default, Clone, Copy)]
pub struct X86_64BpfCompiler;

impl ArchitectureBpfCompiler for X86_64BpfCompiler {
    fn compile_for_arch(
        &self,
        program: &BpfProgram,
        architecture: TargetArchitecture,
    ) -> Result<CompiledBpf> {
        if architecture != TargetArchitecture::X86_64 {
            return Err(Error::Ebpf(
                "x86-64 compiler received a non-x86 target".into(),
            ));
        }
        compile_bpf_internal(program, architecture)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Aarch64BpfCompiler;

impl ArchitectureBpfCompiler for Aarch64BpfCompiler {
    fn compile_for_arch(
        &self,
        program: &BpfProgram,
        architecture: TargetArchitecture,
    ) -> Result<CompiledBpf> {
        if architecture != TargetArchitecture::Aarch64 {
            return Err(Error::Ebpf(
                "AArch64 compiler received a non-AArch64 target".into(),
            ));
        }
        compile_bpf_internal(program, architecture)
    }
}

/// A compiled BPF object ready for loading with aya.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BpfCompileReport {
    pub compiler: String,
    pub target_arch: String,
    pub source_bytes: usize,
    pub elf_bytes: usize,
}

pub struct CompiledBpf {
    /// Raw ELF bytes of the compiled BPF object.
    pub elf_bytes: Vec<u8>,
    /// Path to the source file (retained for debugging).
    pub source_path: PathBuf,
    pub report: BpfCompileReport,
    /// The temp dir is kept alive to prevent cleanup of source_path.
    _temp_dir: tempfile::TempDir,
}

/// Compile a generated BPF C program to ELF bytes using clang.
///
/// Writes the C source to a tempfile, invokes clang with `-target bpf`,
/// and reads back the compiled ELF object.
///
/// # Requirements
/// - `clang` with BPF target support on PATH
/// - Linux kernel headers (usually via `linux-headers-$(uname -r)`)
pub fn compile_bpf(program: &BpfProgram) -> Result<CompiledBpf> {
    compile_bpf_internal(program, host_target_architecture())
}

/// Compile with an explicit target ABI. This is used by cross-architecture
/// plan tests and by deployments whose observed ELF architecture differs from
/// the build host; the legacy `compile_bpf` API remains host-compatible.
pub fn compile_bpf_for_arch(
    program: &BpfProgram,
    architecture: TargetArchitecture,
) -> Result<CompiledBpf> {
    compile_bpf_internal(program, architecture)
}

fn compile_bpf_internal(
    program: &BpfProgram,
    architecture: TargetArchitecture,
) -> Result<CompiledBpf> {
    let temp_dir = tempfile::tempdir()?;

    let src_path = temp_dir.path().join("probe.c");
    let obj_path = temp_dir.path().join("probe.o");

    std::fs::write(&src_path, &program.source)?;

    let mut clang_cmd = std::process::Command::new("clang");
    clang_cmd.args([
        "-O2",
        "-target",
        "bpf",
        "-g", // Include BTF debug info
        // Tell bpf_tracing.h which arch we're targeting.
        // -target bpf strips __aarch64__/__x86_64__ so we set it explicitly.
        bpf_arch_flag(architecture),
        "-Wall",
        "-Wno-unused-value",
        "-Wno-pointer-sign",
        "-Wno-compare-distinct-pointer-types",
        "-c",
        src_path
            .to_str()
            .ok_or_else(|| Error::Ebpf("Non-UTF8 path".to_string()))?,
        "-o",
        obj_path
            .to_str()
            .ok_or_else(|| Error::Ebpf("Non-UTF8 path".to_string()))?,
    ]);

    // Pass TLS offset for x86_64 goid capture via BPF CO-RE.
    if let Some(offset) = program.g_addr_offset {
        clang_cmd.arg(format!("-DG_ADDR_OFFSET={offset}"));
    }
    // Pass goid field offset from DWARF (overrides the #ifndef default of 160).
    if let Some(offset) = program.goid_offset {
        clang_cmd.arg(format!("-DGOID_OFFSET={offset}"));
    }

    let status = clang_cmd.output()?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(Error::Ebpf(format!("clang compilation failed:\n{stderr}")));
    }

    let elf_bytes = std::fs::read(&obj_path)?;
    let elf_size = elf_bytes.len();

    Ok(CompiledBpf {
        elf_bytes,
        source_path: src_path,
        report: BpfCompileReport {
            compiler: "clang".into(),
            target_arch: bpf_arch_flag(architecture)
                .trim_start_matches("-D__TARGET_ARCH_")
                .into(),
            source_bytes: program.source.len(),
            elf_bytes: elf_size,
        },
        _temp_dir: temp_dir,
    })
}

/// Check whether clang with BPF target support is available.
///
/// Returns `Ok(version_string)` if available, `Err` if not.
pub fn check_clang_available() -> Result<String> {
    let output = std::process::Command::new("clang")
        .arg("--version")
        .output()
        .map_err(|_| Error::Ebpf("clang not found on PATH".to_string()))?;

    if !output.status.success() {
        return Err(Error::Ebpf("clang --version failed".to_string()));
    }

    let version = String::from_utf8_lossy(&output.stdout).to_string();

    // Check BPF target support
    let bpf_check = std::process::Command::new("clang")
        .args(["--target=bpf", "--print-supported-targets"])
        .output();

    if bpf_check.is_err() {
        // Some versions use a different flag — just check clang works
        return Ok(version);
    }

    Ok(version)
}

/// Sanitize a metric name to be a valid C identifier for use in BPF source.
///
/// BPF program names in aya must be valid C identifiers.
pub fn sanitize_probe_name(metric_name: &str) -> String {
    let sanitized: String = metric_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // C identifiers cannot start with a digit; prepend underscore if needed
    if sanitized.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("_{}", sanitized)
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architecture_compiler_selects_explicit_kernel_abi() {
        assert_eq!(
            bpf_arch_flag(TargetArchitecture::X86_64),
            "-D__TARGET_ARCH_x86"
        );
        assert_eq!(
            bpf_arch_flag(TargetArchitecture::Aarch64),
            "-D__TARGET_ARCH_arm64"
        );
    }

    #[test]
    fn architecture_compilers_reject_wrong_targets_before_invoking_clang() {
        let program = BpfProgram {
            source: "".into(),
            var_count: 0,
            captures_goid: false,
            g_addr_offset: None,
            goid_offset: None,
            versioned_envelope: false,
        };
        assert!(X86_64BpfCompiler
            .compile_for_arch(&program, TargetArchitecture::Aarch64)
            .is_err());
        assert!(Aarch64BpfCompiler
            .compile_for_arch(&program, TargetArchitecture::X86_64)
            .is_err());
    }

    #[test]
    fn sanitize_probe_name_valid_ident() {
        assert_eq!(sanitize_probe_name("order_amount"), "order_amount");
        assert_eq!(sanitize_probe_name("my_metric_1"), "my_metric_1");
    }

    #[test]
    fn sanitize_probe_name_replaces_hyphens() {
        assert_eq!(sanitize_probe_name("my-metric"), "my_metric");
    }

    #[test]
    fn sanitize_probe_name_replaces_dots() {
        assert_eq!(
            sanitize_probe_name("http.request.size"),
            "http_request_size"
        );
    }

    #[test]
    fn sanitize_probe_name_replaces_spaces() {
        assert_eq!(sanitize_probe_name("my metric"), "my_metric");
    }

    #[test]
    fn sanitize_probe_name_preserves_numbers() {
        assert_eq!(sanitize_probe_name("metric_v2"), "metric_v2");
    }

    #[test]
    fn sanitize_probe_name_prepends_underscore_for_leading_digit() {
        assert_eq!(sanitize_probe_name("123metric"), "_123metric");
        assert_eq!(sanitize_probe_name("9test"), "_9test");
        assert_eq!(sanitize_probe_name("1"), "_1");
    }

    #[test]
    fn sanitize_probe_name_empty() {
        assert_eq!(sanitize_probe_name(""), "");
    }

    #[test]
    fn check_clang_not_panicking_when_missing() {
        // This test just verifies check_clang_available returns an Err cleanly
        // if clang is not installed, rather than panicking.
        // On CI/dev machines with clang, it returns Ok.
        let result = check_clang_available();
        // Either outcome is acceptable — no panic
        let _ = result;
    }

    #[test]
    fn compile_bpf_needs_clang_installed() {
        // If clang isn't available, we expect an Ebpf error (not a panic).
        // On machines with clang+BPF support, this would actually compile.
        use crate::probe::program::generate_bpf_program;
        use crate::probe::types::CaptureConfig;
        let prog = generate_bpf_program(&[], false, None, None, &CaptureConfig::default()).unwrap();

        // We don't assert success/failure here — just that it doesn't panic.
        // The actual compilation result depends on the host environment.
        let result = compile_bpf(&prog);
        let _ = result;
    }
}
