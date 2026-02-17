//! CLI commands for managing daemon credentials.
//!
//! Provides `detrix auth add/list/remove` for managing per-host tokens
//! stored in `~/detrix/credentials.toml`.

use anyhow::Result;
use clap::Subcommand;
use detrix_config::credentials::CredentialsFile;

#[derive(Subcommand)]
pub enum AuthAction {
    /// Add or update credentials for a daemon
    Add {
        /// Daemon host:port (e.g., localhost:8095)
        target: String,

        /// Authentication token
        #[arg(long)]
        token: Option<String>,

        /// Read token from stdin
        #[arg(long)]
        stdin: bool,
    },

    /// List stored credentials (tokens masked)
    List,

    /// Remove credentials for a daemon
    Remove {
        /// Daemon host:port to remove
        target: String,
    },
}

pub async fn run(action: AuthAction) -> Result<()> {
    match action {
        AuthAction::Add {
            target,
            token,
            stdin,
        } => run_add(&target, token, stdin).await,
        AuthAction::List => run_list().await,
        AuthAction::Remove { target } => run_remove(&target).await,
    }
}

async fn run_add(target: &str, token: Option<String>, stdin: bool) -> Result<()> {
    // Validate host:port format
    if !target.contains(':') {
        anyhow::bail!(
            "Invalid target format '{}'. Expected host:port (e.g., localhost:8095)",
            target
        );
    }

    let resolved_token = if stdin {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        buf.trim().to_string()
    } else if let Some(t) = token {
        t
    } else {
        anyhow::bail!("Must provide --token <value> or --stdin");
    };

    if resolved_token.is_empty() {
        anyhow::bail!("Token cannot be empty");
    }

    let mut creds = CredentialsFile::load().map_err(|e| anyhow::anyhow!("{}", e))?;
    creds.add(target, &resolved_token);
    creds.save().map_err(|e| anyhow::anyhow!("{}", e))?;

    println!("Credentials saved for {}", target);
    Ok(())
}

async fn run_list() -> Result<()> {
    let creds = CredentialsFile::load().map_err(|e| anyhow::anyhow!("{}", e))?;

    if creds.targets.is_empty() {
        println!(
            "No credentials stored in {}",
            CredentialsFile::default_path().display()
        );
        return Ok(());
    }

    println!(
        "Stored credentials ({}):",
        CredentialsFile::default_path().display()
    );
    for (target, cred) in &creds.targets {
        let masked = mask_token(&cred.token);
        println!("  {} → {}", target, masked);
    }

    Ok(())
}

async fn run_remove(target: &str) -> Result<()> {
    let mut creds = CredentialsFile::load().map_err(|e| anyhow::anyhow!("{}", e))?;

    if creds.remove(target) {
        creds.save().map_err(|e| anyhow::anyhow!("{}", e))?;
        println!("Credentials removed for {}", target);
    } else {
        println!("No credentials found for {}", target);
    }

    Ok(())
}

/// Mask a token for display: show first 4 chars + "***"
fn mask_token(token: &str) -> String {
    if token.len() <= 4 {
        return "***".to_string();
    }
    format!("{}***", &token[..4])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_token() {
        assert_eq!(mask_token("demo-token"), "demo***");
        assert_eq!(mask_token("ab"), "***");
        assert_eq!(mask_token("abcd"), "***");
        assert_eq!(mask_token("abcde"), "abcd***");
    }
}
