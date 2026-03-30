use std::path::{Path, PathBuf};
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
    pub branch: Option<String>,
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

    /// Git branch to filter GitHub Actions runs (e.g. master, main)
    #[arg(long)]
    pub branch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub aws_profile: String,
    pub region: String,
    pub github_repo: String,
    /// Git branch to filter GitHub Actions runs (None = all branches).
    pub branch: Option<String>,
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

        let branch = cli.branch.or(file.branch);

        let review_workflows = file
            .workflow_categories
            .and_then(|c| c.review)
            .unwrap_or_default();

        Ok(Config {
            aws_profile,
            region,
            github_repo,
            branch,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookLocation {
    /// Installed in .git/hooks/ (no core.hooksPath override).
    Local,
    /// Installed in global hooks dir (core.hooksPath), no delegation.
    Global,
    /// Installed in global hooks dir with delegation to local hooks.
    GlobalDelegated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookStatus {
    /// Snippet found in effective hooks dir — will actually run.
    Installed(HookLocation),
    /// Snippet in .git/hooks/ but core.hooksPath overrides it.
    Shadowed,
    /// Effective hooks dir has a pre-push but no cibars snippet.
    Incomplete,
    /// No pre-push hook in effective hooks dir.
    Missing,
    /// Not a git repository.
    NoGitDir,
}

/// Derive a per-project PID file path from the working directory.
/// Each cibars instance gets its own PID file so hooks target the right process.
pub fn pid_file_for(cwd: &Path) -> Result<PathBuf> {
    let sanitized = cwd.to_string_lossy().replace('/', "_");
    let pids_dir = dirs::home_dir()
        .context("no home dir")?
        .join(".cibars/pids");
    Ok(pids_dir.join(format!("{sanitized}.pid")))
}

/// Dynamic hook snippet — derives PID path at runtime using `pwd`.
/// Works from any directory, targets the correct cibars instance.
const HOOK_SNIPPET: &str = "\n# cibars: boost polling on push\n\
    _cibars_pid=\"$HOME/.cibars/pids/$(pwd | tr '/' '_').pid\"\n\
    kill -USR1 $(cat \"$_cibars_pid\" 2>/dev/null) 2>/dev/null || true\n";

/// Delegation block: calls repo-local pre-push if it exists.
/// Added when core.hooksPath overrides .git/hooks/.
const DELEGATION_SNIPPET: &str = "\n# --- cibars: delegate to repo-local hook ---\n\
    _local=\"$(git rev-parse --git-dir)/hooks/pre-push\"\n\
    if [ -x \"$_local\" ]; then\n    \
    \"$_local\" \"$@\" || exit $?\n\
    fi\n";

fn has_cibars_hook(contents: &str) -> bool {
    contents.contains("USR1") && contents.contains("cibars")
}

fn has_delegation(contents: &str) -> bool {
    contents.contains("rev-parse --git-dir")
        && contents.contains("hooks/pre-push")
        && contents.contains("$_local")
}

/// Resolve effective hooks directory, respecting core.hooksPath.
pub fn effective_hooks_dir(repo: &Path) -> PathBuf {
    let output = Command::new("git")
        .args(["-C", &repo.to_string_lossy(), "config", "core.hooksPath"])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !raw.is_empty() {
                let path = PathBuf::from(&raw);
                if path.is_absolute() {
                    return path;
                }
                // Relative to repo root
                return repo.join(path);
            }
        }
    }
    repo.join(".git/hooks")
}

pub fn check_pre_push_hook(dir: &Path) -> HookStatus {
    let git_dir = dir.join(".git");
    if !git_dir.is_dir() {
        return HookStatus::NoGitDir;
    }
    let effective = effective_hooks_dir(dir);
    let effective_hook = effective.join("pre-push");
    let local_hook = git_dir.join("hooks/pre-push");
    let is_global = effective != git_dir.join("hooks");

    // Check effective hooks dir first
    if let Ok(contents) = std::fs::read_to_string(&effective_hook) {
        if has_cibars_hook(&contents) {
            let location = if !is_global {
                HookLocation::Local
            } else if has_delegation(&contents) {
                HookLocation::GlobalDelegated
            } else {
                HookLocation::Global
            };
            return HookStatus::Installed(location);
        }
        // Effective hook exists but no cibars snippet
        return HookStatus::Incomplete;
    }

    // Effective dir has no pre-push. Check if local hook is shadowed.
    if is_global {
        if let Ok(contents) = std::fs::read_to_string(&local_hook) {
            if has_cibars_hook(&contents) {
                return HookStatus::Shadowed;
            }
        }
        // Global override active, no hook anywhere → Missing
        // (but effective dir is still the right place to install)
        return HookStatus::Missing;
    }

    HookStatus::Missing
}

pub fn install_pre_push_hook(dir: &Path) -> Result<()> {
    let effective = effective_hooks_dir(dir);
    let hook_path = effective.join("pre-push");
    std::fs::create_dir_all(&effective)
        .with_context(|| format!("cannot create {}", effective.display()))?;

    let existing = std::fs::read_to_string(&hook_path).unwrap_or_default();

    // Idempotent: skip if already contains cibars hook
    if has_cibars_hook(&existing) {
        return Ok(());
    }

    let mut additions = String::new();

    // Add delegation if core.hooksPath is active and local hook exists
    let is_global = effective != dir.join(".git/hooks");
    if is_global && !has_delegation(&existing) {
        let local_hook = dir.join(".git/hooks/pre-push");
        if local_hook.is_file() {
            additions.push_str(DELEGATION_SNIPPET);
        }
    }

    additions.push_str(HOOK_SNIPPET);

    let content = if existing.is_empty() {
        format!("#!/bin/sh{additions}")
    } else {
        format!("{existing}{additions}")
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
            branch: None,
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
            branch: None,
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
            branch: None,
            workflow_categories: None,
        };
        let result = Config::merge(&["cibars"], file);
        assert!(result.is_err());
    }

    // --- pid_file_for tests ---

    #[test]
    fn pid_file_for_different_dirs_produce_different_paths() {
        let a = pid_file_for(Path::new("/Users/dev/project-a")).unwrap();
        let b = pid_file_for(Path::new("/Users/dev/project-b")).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn pid_file_for_same_dir_is_deterministic() {
        let a = pid_file_for(Path::new("/Users/dev/myproject")).unwrap();
        let b = pid_file_for(Path::new("/Users/dev/myproject")).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn pid_file_for_lives_under_pids_dir() {
        let p = pid_file_for(Path::new("/Users/dev/myproject")).unwrap();
        assert!(
            p.to_string_lossy().contains(".cibars/pids/"),
            "got: {}",
            p.display()
        );
    }

    #[test]
    fn pid_file_for_ends_with_pid_extension() {
        let p = pid_file_for(Path::new("/Users/dev/myproject")).unwrap();
        assert!(
            p.extension().is_some_and(|e| e == "pid"),
            "got: {}",
            p.display()
        );
    }

    // --- hook snippet tests ---

    #[test]
    fn hook_snippet_uses_dynamic_pwd() {
        assert!(
            HOOK_SNIPPET.contains("$(pwd | tr '/' '_')"),
            "got: {HOOK_SNIPPET}"
        );
    }

    #[test]
    fn hook_snippet_contains_kill_usr1() {
        assert!(HOOK_SNIPPET.contains("kill -USR1"), "got: {HOOK_SNIPPET}");
    }

    #[test]
    fn hook_snippet_references_pids_dir() {
        assert!(
            HOOK_SNIPPET.contains(".cibars/pids/"),
            "got: {HOOK_SNIPPET}"
        );
    }

    // --- has_cibars_hook / has_delegation helpers ---

    #[test]
    fn has_cibars_hook_recognizes_dynamic_snippet() {
        assert!(has_cibars_hook(HOOK_SNIPPET));
    }

    #[test]
    fn has_cibars_hook_recognizes_legacy_pkill() {
        assert!(has_cibars_hook("pkill -USR1 cibars 2>/dev/null"));
    }

    #[test]
    fn has_cibars_hook_rejects_unrelated() {
        assert!(!has_cibars_hook("#!/bin/sh\necho hello\n"));
    }

    #[test]
    fn has_delegation_recognizes_snippet() {
        assert!(has_delegation(DELEGATION_SNIPPET));
    }

    #[test]
    fn has_delegation_rejects_unrelated() {
        assert!(!has_delegation("#!/bin/sh\necho hello\n"));
    }

    // --- effective_hooks_dir tests ---

    /// Helper: init an isolated git repo (blocks global core.hooksPath leaking in).
    fn init_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .unwrap();
        // Override any global core.hooksPath so tests are isolated
        Command::new("git")
            .args([
                "-C",
                &dir.to_string_lossy(),
                "config",
                "--local",
                "--unset",
                "core.hooksPath",
            ])
            .status()
            .ok(); // may fail if not set locally, that's fine
                   // Set explicitly to ensure isolation from global config
        Command::new("git")
            .args([
                "-C",
                &dir.to_string_lossy(),
                "config",
                "--local",
                "core.hooksPath",
                &dir.join(".git/hooks").to_string_lossy(),
            ])
            .status()
            .unwrap();
    }

    /// Helper: init a git repo with a specific core.hooksPath override.
    fn init_git_repo_with_hooks_path(dir: &Path, hooks_path: &Path) {
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                &dir.to_string_lossy(),
                "config",
                "--local",
                "core.hooksPath",
                &hooks_path.to_string_lossy(),
            ])
            .status()
            .unwrap();
    }

    #[test]
    fn effective_hooks_dir_defaults_to_local() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hooks = effective_hooks_dir(dir.path());
        assert_eq!(hooks, dir.path().join(".git/hooks"));
    }

    #[test]
    fn effective_hooks_dir_respects_core_hooks_path() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        let hooks = effective_hooks_dir(dir.path());
        assert_eq!(hooks, global_hooks);
    }

    // --- check_pre_push_hook tests ---

    #[test]
    fn check_hook_no_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::NoGitDir);
    }

    #[test]
    fn check_hook_missing_when_no_hook_file() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Missing);
    }

    #[test]
    fn check_hook_incomplete_when_no_cibars_snippet() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::write(hooks_dir.join("pre-push"), "#!/bin/sh\necho pushing\n").unwrap();
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Incomplete);
    }

    #[test]
    fn check_hook_installed_with_dynamic_snippet() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::write(
            hooks_dir.join("pre-push"),
            format!("#!/bin/sh{HOOK_SNIPPET}"),
        )
        .unwrap();
        assert_eq!(
            check_pre_push_hook(dir.path()),
            HookStatus::Installed(HookLocation::Local)
        );
    }

    #[test]
    fn check_hook_installed_with_legacy_pkill() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::write(
            hooks_dir.join("pre-push"),
            "#!/bin/sh\npkill -USR1 cibars 2>/dev/null\n",
        )
        .unwrap();
        assert_eq!(
            check_pre_push_hook(dir.path()),
            HookStatus::Installed(HookLocation::Local)
        );
    }

    #[test]
    fn check_hook_shadowed_when_local_has_snippet_but_global_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // Local hook has cibars snippet but is shadowed
        let local_hooks = dir.path().join(".git/hooks");
        std::fs::write(
            local_hooks.join("pre-push"),
            format!("#!/bin/sh{HOOK_SNIPPET}"),
        )
        .unwrap();
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Shadowed);
    }

    #[test]
    fn check_hook_installed_in_global_dir() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // Snippet in global dir — this is what matters
        std::fs::write(
            global_hooks.join("pre-push"),
            format!("#!/bin/sh{HOOK_SNIPPET}"),
        )
        .unwrap();
        assert_eq!(
            check_pre_push_hook(dir.path()),
            HookStatus::Installed(HookLocation::Global)
        );
    }

    #[test]
    fn check_hook_installed_in_global_dir_with_delegation() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        std::fs::write(
            global_hooks.join("pre-push"),
            format!("#!/bin/sh{DELEGATION_SNIPPET}{HOOK_SNIPPET}"),
        )
        .unwrap();
        assert_eq!(
            check_pre_push_hook(dir.path()),
            HookStatus::Installed(HookLocation::GlobalDelegated)
        );
    }

    #[test]
    fn check_hook_missing_with_global_override_no_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // No hook anywhere
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Missing);
    }

    // --- install_pre_push_hook tests ---

    #[test]
    fn install_hook_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        install_pre_push_hook(dir.path()).unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert!(content.contains("#!/bin/sh"));
        assert!(content.contains("cibars"));
        assert!(content.contains("$(pwd | tr"));
    }

    #[test]
    fn install_hook_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::write(hooks_dir.join("pre-push"), "#!/bin/sh\necho pushing\n").unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert!(content.contains("echo pushing"));
        assert!(content.contains("cibars"));
    }

    #[test]
    fn install_hook_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        install_pre_push_hook(dir.path()).unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert_eq!(
            content.matches("cibars: boost").count(),
            1,
            "should not duplicate"
        );
    }

    #[test]
    fn install_hook_in_global_dir_when_core_hooks_path() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        install_pre_push_hook(dir.path()).unwrap();
        // Should install in global dir, not local
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(
            content.contains("cibars"),
            "snippet should be in global hooks"
        );
        assert!(
            !dir.path().join(".git/hooks/pre-push").exists()
                || !std::fs::read_to_string(dir.path().join(".git/hooks/pre-push"))
                    .unwrap_or_default()
                    .contains("$(pwd | tr"),
            "should NOT install new snippet in local hooks"
        );
    }

    #[test]
    fn install_hook_adds_delegation_when_local_hook_shadowed() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // Local hook has project-specific content
        let local_hooks = dir.path().join(".git/hooks");
        std::fs::write(
            local_hooks.join("pre-push"),
            "#!/bin/sh\ncd backend && pytest\n",
        )
        .unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(has_delegation(&content), "should add delegation: {content}");
        assert!(
            has_cibars_hook(&content),
            "should add cibars snippet: {content}"
        );
    }

    #[test]
    fn install_hook_skips_delegation_when_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        let local_hooks = dir.path().join(".git/hooks");
        std::fs::write(local_hooks.join("pre-push"), "#!/bin/sh\necho test\n").unwrap();
        // Global hook already has delegation
        std::fs::write(
            global_hooks.join("pre-push"),
            format!("#!/bin/sh{DELEGATION_SNIPPET}"),
        )
        .unwrap();
        install_pre_push_hook(dir.path()).unwrap();
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert_eq!(
            content.matches("rev-parse --git-dir").count(),
            1,
            "should not duplicate delegation: {content}"
        );
    }

    #[test]
    fn install_hook_no_delegation_without_local_hook() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // No local hook
        install_pre_push_hook(dir.path()).unwrap();
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(
            !has_delegation(&content),
            "no delegation without local hook"
        );
        assert!(has_cibars_hook(&content));
    }

    #[cfg(unix)]
    #[test]
    fn install_hook_sets_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        install_pre_push_hook(dir.path()).unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
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
            branch: None,
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
            branch: None,
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

    // --- branch filter tests ---

    #[test]
    fn branch_cli_arg_parses() {
        let config = Config::try_from_args(&[
            "cibars",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
            "--branch",
            "master",
        ])
        .unwrap();
        assert_eq!(config.branch, Some("master".to_string()));
    }

    #[test]
    fn branch_from_file_config() {
        let file = FileConfig {
            aws_profile: Some("p".into()),
            region: Some("r".into()),
            github_repo: Some("o/r".into()),
            branch: Some("master".into()),
            workflow_categories: None,
        };
        let config = Config::merge(&["cibars"], file).unwrap();
        assert_eq!(config.branch, Some("master".to_string()));
    }

    #[test]
    fn branch_cli_overrides_file() {
        let file = FileConfig {
            aws_profile: Some("p".into()),
            region: Some("r".into()),
            github_repo: Some("o/r".into()),
            branch: Some("develop".into()),
            workflow_categories: None,
        };
        let config = Config::merge(&["cibars", "--branch", "main"], file).unwrap();
        assert_eq!(config.branch, Some("main".to_string()));
    }

    #[test]
    fn branch_defaults_to_none() {
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
        assert!(config.branch.is_none());
    }

    #[test]
    fn file_config_parses_branch_toml() {
        let toml_str = r#"
aws_profile = "p"
region = "r"
github_repo = "o/r"
branch = "master"
"#;
        let fc: FileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(fc.branch.unwrap(), "master");
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
