use anyhow::{ensure, Context, Result};
use clap::Parser;

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
        let token = std::env::var("GITHUB_TOKEN")
            .context("GITHUB_TOKEN environment variable is required")?;
        ensure!(
            config.github_repo.contains('/'),
            "github-repo must be in owner/repo format"
        );
        Ok((config, token))
    }

    /// For testing: parse from args without env var
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
