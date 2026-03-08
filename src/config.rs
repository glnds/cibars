use std::path::Path;
use std::process::Command;

use anyhow::{ensure, Context, Result};
use clap::Parser;
use serde::Deserialize;

use crate::model::WorkflowCategory;

#[derive(Deserialize, Debug, Default)]
pub struct WorkflowCategoryConfig {
    pub review: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Default)]
pub struct FileConfig {
    pub aws_profile: Option<String>,
    pub region: Option<String>,
    pub github_repo: Option<String>,
    pub workflow_categories: Option<WorkflowCategoryConfig>,
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
    /// Explicit workflow names classified as Review (from config file).
    pub review_workflows: Vec<String>,
}

impl Config {
    pub fn load(cwd: &Path) -> Result<(Self, String)> {
        let cli = CliArgs::parse();
        let file = load_file_config(cwd);
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

        let review_workflows = file
            .workflow_categories
            .and_then(|c| c.review)
            .unwrap_or_default();

        Ok(Config {
            aws_profile,
            region,
            github_repo,
            review_workflows,
        })
    }

    /// Classify a workflow name as CI or Review.
    /// Config overrides take precedence over heuristics.
    pub fn classify_workflow(&self, name: &str) -> WorkflowCategory {
        // Config override: exact match (case-sensitive)
        if self.review_workflows.iter().any(|r| r == name) {
            return WorkflowCategory::Review;
        }

        // Auto-detect heuristics (case-insensitive)
        let lower = name.to_lowercase();
        if lower.contains("review")
            || lower.contains("dependabot")
            || lower.contains("labeler")
            || lower.contains("stale")
        {
            return WorkflowCategory::Review;
        }

        WorkflowCategory::CI
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookStatus {
    Installed,
    Incomplete,
    Missing,
    NoGitDir,
}

pub fn check_pre_push_hook(dir: &Path) -> HookStatus {
    let git_dir = dir.join(".git");
    if !git_dir.is_dir() {
        return HookStatus::NoGitDir;
    }
    let hook_path = git_dir.join("hooks/pre-push");
    match std::fs::read_to_string(&hook_path) {
        Ok(contents) => {
            if contents.contains("USR1") && contents.contains("cibars") {
                HookStatus::Installed
            } else {
                HookStatus::Incomplete
            }
        }
        Err(_) => HookStatus::Missing,
    }
}

const HOOK_SNIPPET: &str =
    "\n# cibars: boost polling on push\npkill -USR1 cibars 2>/dev/null || true\n";

pub fn install_pre_push_hook(dir: &Path) -> Result<()> {
    let hook_path = dir.join(".git/hooks/pre-push");
    let hooks_dir = dir.join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("cannot create {}", hooks_dir.display()))?;

    let existing = std::fs::read_to_string(&hook_path).unwrap_or_default();

    // Idempotent: skip if already contains the boost command
    if existing.contains("USR1") && existing.contains("cibars") {
        return Ok(());
    }

    let content = if existing.is_empty() {
        format!("#!/bin/sh{HOOK_SNIPPET}")
    } else {
        format!("{existing}{HOOK_SNIPPET}")
    };

    std::fs::write(&hook_path, content)
        .with_context(|| format!("cannot write {}", hook_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    Ok(())
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
    use crate::model::WorkflowCategory;

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
            workflow_categories: None,
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
            workflow_categories: None,
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
            workflow_categories: None,
        };
        let result = Config::merge(&["cibars"], file);
        assert!(result.is_err());
    }

    #[test]
    fn hook_status_missing_when_no_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::NoGitDir);
    }

    #[test]
    fn hook_status_missing_when_no_hook_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Missing);
    }

    #[test]
    fn hook_status_incomplete_when_no_boost_command() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(hooks_dir.join("pre-push"), "#!/bin/sh\necho pushing\n").unwrap();
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Incomplete);
    }

    #[test]
    fn hook_status_installed_when_boost_present() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("pre-push"),
            "#!/bin/sh\npkill -USR1 cibars 2>/dev/null\nexit 0\n",
        )
        .unwrap();
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Installed);
    }

    #[test]
    fn hook_status_installed_with_variant_commands() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        // kill -USR1 variant (not pkill)
        std::fs::write(
            hooks_dir.join("pre-push"),
            "#!/bin/sh\nkill -USR1 $(pgrep cibars)\n",
        )
        .unwrap();
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Installed);
    }

    #[test]
    fn install_hook_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert!(content.contains("#!/bin/sh"));
        assert!(content.contains("pkill -USR1 cibars"));
    }

    #[test]
    fn install_hook_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(hooks_dir.join("pre-push"), "#!/bin/sh\necho pushing\n").unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert!(content.contains("echo pushing"));
        assert!(content.contains("pkill -USR1 cibars"));
    }

    #[test]
    fn install_hook_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert_eq!(content.matches("pkill -USR1 cibars").count(), 1);
    }

    #[test]
    fn install_hook_idempotent_with_variant_command() {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        // Pre-existing hook uses kill variant instead of pkill
        std::fs::write(
            hooks_dir.join("pre-push"),
            "#!/bin/sh\nkill -USR1 $(pgrep cibars)\n",
        )
        .unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        // Should NOT append another snippet since USR1+cibars already present
        assert!(!content.contains("pkill -USR1 cibars"));
        assert!(content.contains("kill -USR1 $(pgrep cibars)"));
    }

    #[cfg(unix)]
    #[test]
    fn install_hook_sets_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let perms = std::fs::metadata(hooks_dir.join("pre-push"))
            .unwrap()
            .permissions();
        assert!(perms.mode() & 0o111 != 0, "hook should be executable");
    }

    // --- classify_workflow tests ---

    #[test]
    fn classify_auto_review_pattern() {
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
        ])
        .unwrap();
        assert_eq!(
            config.classify_workflow("Claude Code Review"),
            WorkflowCategory::Review
        );
    }

    #[test]
    fn classify_auto_ci_pattern() {
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
        ])
        .unwrap();
        assert_eq!(config.classify_workflow("CI"), WorkflowCategory::CI);
        assert_eq!(
            config.classify_workflow("Security Audit"),
            WorkflowCategory::CI
        );
    }

    #[test]
    fn classify_auto_dependabot() {
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
        ])
        .unwrap();
        assert_eq!(
            config.classify_workflow("dependabot"),
            WorkflowCategory::Review
        );
    }

    #[test]
    fn classify_auto_labeler() {
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
        ])
        .unwrap();
        assert_eq!(
            config.classify_workflow("PR Labeler"),
            WorkflowCategory::Review
        );
    }

    #[test]
    fn classify_auto_stale() {
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
        ])
        .unwrap();
        assert_eq!(
            config.classify_workflow("Mark stale issues"),
            WorkflowCategory::Review
        );
    }

    #[test]
    fn classify_config_override_takes_precedence() {
        let file = FileConfig {
            aws_profile: Some("p".into()),
            region: Some("r".into()),
            github_repo: Some("o/r".into()),
            workflow_categories: Some(WorkflowCategoryConfig {
                review: Some(vec!["My Custom Workflow".into()]),
            }),
        };
        let config = Config::merge(&["cibars"], file).unwrap();
        assert_eq!(
            config.classify_workflow("My Custom Workflow"),
            WorkflowCategory::Review
        );
    }

    #[test]
    fn classify_no_workflow_categories_section() {
        let file = FileConfig {
            aws_profile: Some("p".into()),
            region: Some("r".into()),
            github_repo: Some("o/r".into()),
            workflow_categories: None,
        };
        let config = Config::merge(&["cibars"], file).unwrap();
        assert_eq!(config.classify_workflow("CI"), WorkflowCategory::CI);
        assert_eq!(
            config.classify_workflow("Claude Code Review"),
            WorkflowCategory::Review
        );
    }

    #[test]
    fn classify_config_toml_parses_workflow_categories() {
        let toml_str = r#"
aws_profile = "p"
region = "r"
github_repo = "o/r"

[workflow_categories]
review = ["Claude Code Review", "dependabot"]
"#;
        let fc: FileConfig = toml::from_str(toml_str).unwrap();
        assert!(fc.workflow_categories.is_some());
        let cats = fc.workflow_categories.unwrap();
        assert_eq!(
            cats.review.unwrap(),
            vec!["Claude Code Review", "dependabot"]
        );
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
