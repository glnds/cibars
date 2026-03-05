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
pub struct CliArgs {
    /// AWS profile name
    #[arg(long)]
    pub aws_profile: Option<String>,

    /// AWS region
    #[arg(long)]
    pub region: Option<String>,

    /// GitHub repository (owner/repo)
    #[arg(long)]
    pub github_repo: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub aws_profile: String,
    pub region: String,
    pub github_repo: String,
}

impl Config {
    pub fn load() -> Result<(Self, String)> {
        let cli = CliArgs::parse();
        let file = load_file_config(&std::env::current_dir().context("cannot read cwd")?);
        let config = Self::merge_sources(cli, file)?;
        let token = resolve_github_token()?;
        Ok((config, token))
    }

    fn merge_sources(cli: CliArgs, file: FileConfig) -> Result<Self> {
        let aws_profile = cli
            .aws_profile
            .or(file.aws_profile)
            .context("aws_profile: not provided via --aws-profile or config.toml")?;
        let region = cli
            .region
            .or(file.region)
            .context("region: not provided via --region or config.toml")?;
        let github_repo = cli
            .github_repo
            .or(file.github_repo)
            .context("github_repo: not provided via --github-repo or config.toml")?;

        ensure!(
            github_repo.contains('/'),
            "github-repo must be in owner/repo format"
        );

        Ok(Config {
            aws_profile,
            region,
            github_repo,
        })
    }

    #[cfg(test)]
    fn merge(args: &[&str], file: FileConfig) -> Result<Self> {
        let cli = CliArgs::try_parse_from(args)?;
        Self::merge_sources(cli, file)
    }

    #[cfg(test)]
    pub fn try_from_args(args: &[&str]) -> Result<Self> {
        Self::merge(args, FileConfig::default())
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

fn load_file_config(dir: &Path) -> FileConfig {
    let path = dir.join("config.toml");
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            tracing::info!("loaded config from {}", path.display());
            toml::from_str(&contents).unwrap_or_else(|e| {
                tracing::warn!("failed to parse {}: {e}", path.display());
                FileConfig::default()
            })
        }
        Err(_) => FileConfig::default(),
    }
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

    use std::io::Write;

    #[test]
    fn load_file_config_reads_toml_from_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&config_path).unwrap();
        write!(
            f,
            "aws_profile = \"prod\"\nregion = \"us-east-1\"\ngithub_repo = \"org/repo\""
        )
        .unwrap();

        let fc = load_file_config(dir.path());
        assert_eq!(fc.aws_profile.unwrap(), "prod");
        assert_eq!(fc.region.unwrap(), "us-east-1");
        assert_eq!(fc.github_repo.unwrap(), "org/repo");
    }

    #[test]
    fn load_file_config_returns_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let fc = load_file_config(dir.path());
        assert!(fc.aws_profile.is_none());
        assert!(fc.region.is_none());
        assert!(fc.github_repo.is_none());
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
    fn merge_cli_overrides_file_config() {
        let file = FileConfig {
            aws_profile: Some("from-file".into()),
            region: Some("eu-west-1".into()),
            github_repo: Some("org/repo".into()),
        };
        let config = Config::merge(&["cibars", "--aws-profile", "from-cli"], file).unwrap();
        assert_eq!(config.aws_profile, "from-cli");
        assert_eq!(config.region, "eu-west-1");
        assert_eq!(config.github_repo, "org/repo");
    }

    #[test]
    fn merge_file_only_no_cli_args() {
        let file = FileConfig {
            aws_profile: Some("staging".into()),
            region: Some("eu-west-1".into()),
            github_repo: Some("acme/backend".into()),
        };
        let config = Config::merge(&["cibars"], file).unwrap();
        assert_eq!(config.aws_profile, "staging");
        assert_eq!(config.region, "eu-west-1");
        assert_eq!(config.github_repo, "acme/backend");
    }

    #[test]
    fn merge_cli_only_no_file() {
        let file = FileConfig::default();
        let config = Config::merge(
            &[
                "cibars",
                "--aws-profile",
                "p",
                "--region",
                "r",
                "--github-repo",
                "o/r",
            ],
            file,
        )
        .unwrap();
        assert_eq!(config.aws_profile, "p");
        assert_eq!(config.region, "r");
        assert_eq!(config.github_repo, "o/r");
    }

    #[test]
    fn merge_missing_field_errors() {
        let file = FileConfig {
            aws_profile: Some("staging".into()),
            region: None,
            github_repo: None,
        };
        let result = Config::merge(&["cibars"], file);
        assert!(result.is_err());
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
