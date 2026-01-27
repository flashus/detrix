//! CodeLLDB Automatic Installer
//!
//! Downloads and installs CodeLLDB from GitHub releases.
//! CodeLLDB has better PDB support on Windows compared to standard lldb-dap.
//!
//! Based on Zed's implementation in dap_adapters/src/codelldb.rs
//!
//! This module's functions are primarily used on Windows. On other platforms,
//! standard lldb-dap is preferred.

#![allow(dead_code)] // Functions are conditionally used based on target platform

use anyhow::{Context, Result};
use detrix_logging::{debug, info};
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};

const CODELLDB_REPO_OWNER: &str = "vadimcn";
const CODELLDB_REPO_NAME: &str = "codelldb";
const ADAPTER_NAME: &str = "CodeLLDB";

/// Version information for a GitHub release
#[derive(Debug, Clone)]
pub struct AdapterVersion {
    pub tag_name: String,
    pub url: String,
}

/// GitHub release asset
#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// GitHub release response
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

/// Get the data directory for storing adapters
fn get_adapters_dir() -> Result<PathBuf> {
    // Use platform-specific data directory
    #[cfg(target_os = "windows")]
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("USERPROFILE")
                .map(|h| PathBuf::from(h).join("AppData").join("Local"))
                .unwrap_or_else(|_| PathBuf::from("."))
        });

    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
        .unwrap_or_else(|_| PathBuf::from("."));

    #[cfg(target_os = "linux")]
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|_| PathBuf::from("."));

    let adapters_dir = base.join("detrix").join("debug_adapters");
    Ok(adapters_dir)
}

/// Fetch the latest GitHub release for CodeLLDB
fn fetch_latest_release() -> Result<GitHubRelease> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        CODELLDB_REPO_OWNER, CODELLDB_REPO_NAME
    );

    debug!("Fetching latest CodeLLDB release from: {}", url);

    let client = reqwest::blocking::Client::builder()
        .user_agent("detrix")
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(&url)
        .send()
        .context("Failed to fetch GitHub release")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "GitHub API returned status {}: {}",
            response.status(),
            response.text().unwrap_or_default()
        );
    }

    let release: GitHubRelease = response.json().context("Failed to parse GitHub release")?;
    debug!("Latest CodeLLDB release: {}", release.tag_name);

    Ok(release)
}

/// Get the appropriate asset name for the current platform
fn get_asset_name() -> Result<String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        unsupported => anyhow::bail!("Unsupported architecture: {}", unsupported),
    };

    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "win32",
        unsupported => anyhow::bail!("Unsupported operating system: {}", unsupported),
    };

    Ok(format!("codelldb-{}-{}.vsix", platform, arch))
}

/// Download a file from URL to the given path
fn download_file(url: &str, dest: &PathBuf) -> Result<()> {
    info!("Downloading CodeLLDB from: {}", url);

    let client = reqwest::blocking::Client::builder()
        .user_agent("detrix")
        .build()
        .context("Failed to create HTTP client")?;

    let response = client.get(url).send().context("Failed to download file")?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed with status {}", response.status());
    }

    let bytes = response.bytes().context("Failed to read response body")?;
    let mut file = File::create(dest).context("Failed to create destination file")?;
    file.write_all(&bytes)
        .context("Failed to write downloaded content")?;

    info!("Downloaded {} bytes to {:?}", bytes.len(), dest);
    Ok(())
}

/// Extract a .vsix file (which is a ZIP archive)
fn extract_vsix(vsix_path: &PathBuf, dest_dir: &PathBuf) -> Result<()> {
    debug!("Extracting {:?} to {:?}", vsix_path, dest_dir);

    let file = File::open(vsix_path).context("Failed to open VSIX file")?;
    let reader = BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader).context("Failed to read ZIP archive")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = match file.enclosed_name() {
            Some(path) => dest_dir.join(path),
            None => continue,
        };

        if file.is_dir() {
            fs::create_dir_all(&outpath).ok();
        } else {
            if let Some(p) = outpath.parent() {
                fs::create_dir_all(p).ok();
            }
            let mut outfile = File::create(&outpath)?;
            io::copy(&mut file, &mut outfile)?;

            // Set executable permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if outpath.to_string_lossy().contains("codelldb")
                    || outpath.to_string_lossy().contains("lldb")
                {
                    let mut perms = fs::metadata(&outpath)?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&outpath, perms)?;
                }
            }
        }
    }

    debug!("Extraction complete");
    Ok(())
}

/// Get the path to the CodeLLDB adapter binary
fn get_adapter_binary_path(version_dir: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    let binary_name = "codelldb.exe";
    #[cfg(not(target_os = "windows"))]
    let binary_name = "codelldb";

    version_dir
        .join("extension")
        .join("adapter")
        .join(binary_name)
}

/// Check if a version of CodeLLDB is already installed
fn find_installed_version(adapters_dir: &Path) -> Option<PathBuf> {
    let adapter_dir = adapters_dir.join(ADAPTER_NAME);
    if !adapter_dir.exists() {
        return None;
    }

    // Look for any version directory
    if let Ok(entries) = fs::read_dir(&adapter_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let binary_path = get_adapter_binary_path(&path);
                if binary_path.exists() {
                    return Some(binary_path);
                }
            }
        }
    }

    None
}

/// Remove old versions of the adapter
fn remove_old_versions(adapter_dir: &PathBuf, current_version: &str) -> Result<()> {
    if !adapter_dir.exists() {
        return Ok(());
    }

    let current_dir_name = format!("{}_{}", ADAPTER_NAME, current_version);

    if let Ok(entries) = fs::read_dir(adapter_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let dir_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if dir_name != current_dir_name && !dir_name.ends_with(".zip") {
                    debug!("Removing old version: {:?}", path);
                    fs::remove_dir_all(&path).ok();
                }
            }
        }
    }

    // Also remove any leftover .zip files
    if let Ok(entries) = fs::read_dir(adapter_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "zip").unwrap_or(false) {
                debug!("Removing leftover zip: {:?}", path);
                fs::remove_file(&path).ok();
            }
        }
    }

    Ok(())
}

/// Download and install CodeLLDB from GitHub
///
/// Returns the path to the codelldb binary
pub fn ensure_codelldb_installed() -> Result<PathBuf> {
    let adapters_dir = get_adapters_dir()?;
    let adapter_dir = adapters_dir.join(ADAPTER_NAME);

    // First, check if we already have a working installation
    if let Some(existing) = find_installed_version(&adapters_dir) {
        info!("Found existing CodeLLDB installation: {:?}", existing);
        return Ok(existing);
    }

    info!("CodeLLDB not found locally, downloading from GitHub...");

    // Fetch latest release info
    let release = fetch_latest_release()?;
    let asset_name = get_asset_name()?;

    // Find the matching asset
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .with_context(|| format!("No asset found matching {}", asset_name))?;

    info!(
        "Downloading CodeLLDB {} ({})...",
        release.tag_name, asset_name
    );

    // Create directories
    fs::create_dir_all(&adapter_dir).context("Failed to create adapter directory")?;

    let version_dir = adapter_dir.join(format!("{}_{}", ADAPTER_NAME, release.tag_name));

    // Skip download if this exact version already exists
    if version_dir.exists() {
        let binary_path = get_adapter_binary_path(&version_dir);
        if binary_path.exists() {
            info!(
                "CodeLLDB {} already installed at {:?}",
                release.tag_name, binary_path
            );
            return Ok(binary_path);
        }
    }

    // Download the .vsix file
    let vsix_path = adapter_dir.join(format!("{}.vsix", release.tag_name));
    download_file(&asset.browser_download_url, &vsix_path)?;

    // Extract
    info!("Extracting CodeLLDB...");
    extract_vsix(&vsix_path, &version_dir)?;

    // Clean up the .vsix file
    fs::remove_file(&vsix_path).ok();

    // Get the binary path
    let binary_path = get_adapter_binary_path(&version_dir);
    if !binary_path.exists() {
        anyhow::bail!(
            "CodeLLDB binary not found after extraction at {:?}",
            binary_path
        );
    }

    // Remove old versions
    remove_old_versions(&adapter_dir, &release.tag_name)?;

    info!(
        "CodeLLDB {} installed successfully at {:?}",
        release.tag_name, binary_path
    );
    Ok(binary_path)
}

/// Try to find or install CodeLLDB
///
/// First checks VSCode extensions, then tries to download from GitHub
pub fn find_or_install_codelldb() -> Result<PathBuf> {
    // First, try to find in VSCode extensions (like we did before)
    if let Some(path) = find_codelldb_in_vscode() {
        info!("Found CodeLLDB in VSCode extensions: {:?}", path);
        return Ok(path);
    }

    // If not found, download and install
    ensure_codelldb_installed()
}

/// Find CodeLLDB in VSCode extensions directories
fn find_codelldb_in_vscode() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let home = std::env::var("USERPROFILE").ok()?;

        let extension_dirs = [
            format!("{}/.vscode/extensions", home),
            format!("{}/.vscode-insiders/extensions", home),
        ];

        for ext_dir in &extension_dirs {
            let ext_path = PathBuf::from(ext_dir);
            if !ext_path.exists() {
                continue;
            }

            if let Ok(entries) = fs::read_dir(&ext_path) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with("vadimcn.vscode-lldb") {
                        // CodeLLDB stores its adapter in the adapter directory
                        let adapter_path = entry.path().join("adapter").join("codelldb.exe");
                        debug!("Checking CodeLLDB adapter at: {:?}", adapter_path);
                        if adapter_path.exists() {
                            return Some(adapter_path);
                        }

                        // Also try lldb-dap in the lldb/bin directory (newer versions)
                        let lldb_dap_path =
                            entry.path().join("lldb").join("bin").join("lldb-dap.exe");
                        debug!("Checking CodeLLDB lldb-dap at: {:?}", lldb_dap_path);
                        if lldb_dap_path.exists() {
                            return Some(lldb_dap_path);
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let extension_dirs = [
                format!("{}/.vscode/extensions", home),
                format!("{}/.vscode-insiders/extensions", home),
            ];

            for ext_dir in &extension_dirs {
                let ext_path = PathBuf::from(ext_dir);
                if !ext_path.exists() {
                    continue;
                }

                if let Ok(entries) = fs::read_dir(&ext_path) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if name_str.starts_with("vadimcn.vscode-lldb") {
                            let adapter_path = entry.path().join("adapter").join("codelldb");
                            debug!("Checking CodeLLDB adapter at: {:?}", adapter_path);
                            if adapter_path.exists() {
                                return Some(adapter_path);
                            }
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let extension_dirs = [
                format!("{}/.vscode/extensions", home),
                format!("{}/.vscode-insiders/extensions", home),
                format!("{}/.vscode-oss/extensions", home),
            ];

            for ext_dir in &extension_dirs {
                let ext_path = PathBuf::from(ext_dir);
                if !ext_path.exists() {
                    continue;
                }

                if let Ok(entries) = fs::read_dir(&ext_path) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if name_str.starts_with("vadimcn.vscode-lldb") {
                            let adapter_path = entry.path().join("adapter").join("codelldb");
                            debug!("Checking CodeLLDB adapter at: {:?}", adapter_path);
                            if adapter_path.exists() {
                                return Some(adapter_path);
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_asset_name() {
        let name = get_asset_name().unwrap();
        assert!(name.starts_with("codelldb-"));
        assert!(name.ends_with(".vsix"));
    }

    #[test]
    fn test_get_adapters_dir() {
        let dir = get_adapters_dir().unwrap();
        assert!(dir.to_string_lossy().contains("detrix"));
        assert!(dir.to_string_lossy().contains("debug_adapters"));
    }
}
