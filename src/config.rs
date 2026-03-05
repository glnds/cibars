use std::path::Path;
use std::process::Command;

use anyhow::{ensure, Context, Result};
use clap::Parser;
use serde::Deserialize;

#[derive(Deserialize, Debug, Default)]
pub struct FileConfig {
    pub aws_profile: Option<String>,
    pub region: Option<String>,
    pub github_repo: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "cibars", about = "CI build status bars")]
pub struct Config {
    /// AWS profile name
    #[arg(long)]
    pub aws_profile: String,

    /// AWS region
    #[arg(long)]
    pub region: String,

    /// GitHub repository (owner/repo)
    #[arg(long)]
    pub github_repo: String,
}

impl Config {
    pub fn load() -> Result<(Self, String)> {
        let config = Self::parse();
        let token = resolve_github_token()?;
        ensure!(
            config.github_repo.contains('/'),
            "github-repo must be in owner/repo format"
        );
        Ok((config, token))
    }

    #[cfg(test)]
    pub fn try_from_args(args: &[&str]) -> Result<Self> {
        let config = Self::try_parse_from(args)?;
        ensure!(
            config.github_repo.contains('/'),
            "github-repo must be in owner/repo format"
        );
        Ok(config)
    }
}

/// Resolve GitHub token: GITHUB_TOKEN env var, then `gh auth token`.
fn resolve_github_token() -> Result<String> {
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        return Ok(token);
    }

    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("GITHUB_TOKEN not set and `gh` CLI not found")?;

    if !output.status.success() {
        anyhow::bail!("GITHUB_TOKEN not set and `gh auth token` failed (not logged in?)");
    }

    let token = String::from_utf8(output.stdout)
        .context("invalid UTF-8 from gh auth token")?
        .trim()
        .to_string();

    ensure!(
        !token.is_empty(),
        "GITHUB_TOKEN not set and `gh auth token` returned empty"
    );

    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_args_parse() {
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "staging",
            "--region",
            "eu-west-1",
            "--github-repo",
            "acme/backend",
        ])
        .unwrap();
        assert_eq!(config.aws_profile, "staging");
        assert_eq!(config.region, "eu-west-1");
        assert_eq!(config.github_repo, "acme/backend");
    }

    #[test]
    fn missing_profile_fails() {
        let result = Config::try_from_args(&[
            "cibars",
            "--region",
            "eu-west-1",
            "--github-repo",
            "acme/backend",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn missing_region_fails() {
        let result = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "staging",
            "--github-repo",
            "acme/backend",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn missing_repo_fails() {
        let result = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "staging",
            "--region",
            "eu-west-1",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn file_config_parses_full_toml() {
        let toml_str = r#"
aws_profile = "staging"
region = "eu-west-1"
github_repo = "acme/backend"
"#;
        let fc: FileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(fc.aws_profile.unwrap(), "staging");
        assert_eq!(fc.region.unwrap(), "eu-west-1");
        assert_eq!(fc.github_repo.unwrap(), "acme/backend");
    }

    #[test]
    fn file_config_parses_partial_toml() {
        let toml_str = r#"
aws_profile = "staging"
"#;
        let fc: FileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(fc.aws_profile.unwrap(), "staging");
        assert!(fc.region.is_none());
        assert!(fc.github_repo.is_none());
    }

    #[test]
    fn file_config_parses_empty_toml() {
        let fc: FileConfig = toml::from_str("").unwrap();
        assert!(fc.aws_profile.is_none());
        assert!(fc.region.is_none());
        assert!(fc.github_repo.is_none());
    }

    #[test]
    fn invalid_repo_format_fails() {
        let result = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "staging",
            "--region",
            "eu-west-1",
            "--github-repo",
            "no-slash-here",
        ]);
        assert!(result.is_err());
    }
}
