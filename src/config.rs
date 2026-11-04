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
    /// Which CI sources to monitor: "github", "aws". Absent = both.
    pub sources: Option<Vec<String>>,
    pub aws_profile: Option<String>,
    pub region: Option<String>,
    pub github_repo: Option<String>,
    pub branch: Option<String>,
    pub workflow_categories: Option<WorkflowCategoryConfig>,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "cibars", about = "CI build status bars")]
pub struct CliArgs {
    /// CI sources to monitor, comma-separated: github,aws (default: both)
    #[arg(long, value_delimiter = ',')]
    pub sources: Option<Vec<String>>,

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

/// AWS CodePipeline source settings.
#[derive(Debug, Clone)]
pub struct AwsSource {
    pub profile: String,
    pub region: String,
}

/// GitHub Actions source settings.
#[derive(Debug, Clone)]
pub struct GithubSource {
    /// owner/repo
    pub repo: String,
    /// Git branch to filter GitHub Actions runs (None = all branches).
    pub branch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// None = AWS CodePipeline monitoring disabled.
    pub aws: Option<AwsSource>,
    /// None = GitHub Actions monitoring disabled.
    pub github: Option<GithubSource>,
    /// Explicit workflow names classified as Review (from config file).
    pub review_workflows: Vec<String>,
}

const SOURCE_GITHUB: &str = "github";
const SOURCE_AWS: &str = "aws";

impl Config {
    /// Returns the config and, when GitHub is enabled, its token.
    pub fn load(cwd: &Path) -> Result<(Self, Option<String>)> {
        let cli = CliArgs::parse();
        let file = load_file_config(cwd);
        let config = Self::merge_sources(cli, file)?;
        let token = match config.github {
            Some(_) => Some(resolve_github_token()?),
            None => None,
        };
        Ok((config, token))
    }

    fn merge_sources(cli: CliArgs, file: FileConfig) -> Result<Self> {
        // Absent everywhere = both (backward compatible).
        let sources = cli
            .sources
            .or(file.sources)
            .unwrap_or_else(|| vec![SOURCE_GITHUB.into(), SOURCE_AWS.into()]);
        ensure!(
            !sources.is_empty(),
            "sources: empty; set at least one of \"github\", \"aws\""
        );
        if let Some(bad) = sources
            .iter()
            .find(|s| *s != SOURCE_GITHUB && *s != SOURCE_AWS)
        {
            anyhow::bail!("sources: unknown source {bad:?}; expected \"github\" or \"aws\"");
        }
        let want = |name: &str| sources.iter().any(|s| s == name);

        let aws = if want(SOURCE_AWS) {
            let profile = cli
                .aws_profile
                .or(file.aws_profile)
                .context("aws_profile: not provided via --aws-profile or config.toml (needed by source \"aws\")")?;
            let region = cli.region.or(file.region).context(
                "region: not provided via --region or config.toml (needed by source \"aws\")",
            )?;
            Some(AwsSource { profile, region })
        } else {
            None
        };

        let github = if want(SOURCE_GITHUB) {
            let repo = cli
                .github_repo
                .or(file.github_repo)
                .context("github_repo: not provided via --github-repo or config.toml (needed by source \"github\")")?;
            ensure!(
                repo.contains('/'),
                "github-repo must be in owner/repo format"
            );
            Some(GithubSource {
                repo,
                branch: cli.branch.or(file.branch),
            })
        } else {
            None
        };

        let review_workflows = file
            .workflow_categories
            .and_then(|c| c.review)
            .unwrap_or_default();

        Ok(Config {
            aws,
            github,
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
    /// Installed in global hooks dir (core.hooksPath), always delegates to local.
    Global,
}

/// Target location for hook installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTarget {
    /// Install in .git/hooks/ (repo-local).
    Local,
    /// Install in global hooks dir (core.hooksPath), always with delegation.
    Global,
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

/// Dynamic hook snippet — derives PID path at runtime using the repository root.
/// Works from any directory, targets the correct cibars instance.
const HOOK_SNIPPET: &str = "\n# cibars: boost polling on push\n\
    _cibars_pid=\"$HOME/.cibars/pids/$(git rev-parse --show-toplevel | tr '/' '_').pid\"\n\
    kill -USR1 $(cat \"$_cibars_pid\" 2>/dev/null) 2>/dev/null || true\n";

/// Legacy delegation block, retained for migration to the stdin-replaying wrapper.
const DELEGATION_SNIPPET: &str = "\n# --- cibars: delegate to repo-local hook ---\n\
    _local=\"$(git rev-parse --git-dir)/hooks/pre-push\"\n\
    if [ -x \"$_local\" ]; then\n    \
    \"$_local\" \"$@\" || exit $?\n\
    fi\n";

const HOOK_WRAPPER_HEADER: &str = "#!/bin/sh\n# cibars: managed pre-push wrapper\n";
const ORIGINAL_DELEGATION_MARKER: &str =
    "# cibars: preserved original delegates to repo-local hook\n";
const ORIGINAL_HOOK_SNIPPET: &str =
    "\"$0.cibars-original\" \"$@\" < \"$_cibars_stdin\" || exit $?\n";
const REPLAY_DELEGATION_SNIPPET: &str = "\n# --- cibars: delegate to repo-local hook ---\n\
    _local=\"$(git rev-parse --git-dir)/hooks/pre-push\"\n\
    if [ -x \"$_local\" ] && ! [ \"$_local\" -ef \"$0\" ]; then\n    \
    \"$_local\" \"$@\" < \"$_cibars_stdin\" || exit $?\n\
    fi\n";

fn read_hook(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn make_hook_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(permissions.mode() | 0o100);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn create_private_hook(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Run existing hooks separately: their shebang, exit/exec, and status remain authoritative.
/// Preserve known older snippets by moving their behavior into the wrapper.
fn install_hook_wrapper(path: &Path, boost: bool, delegate: bool) -> Result<bool> {
    use std::io::Write;

    let existing = read_hook(path)?;
    let managed = existing.starts_with(HOOK_WRAPPER_HEADER);
    let boost = boost || has_cibars_hook(&existing);
    let delegate = delegate || has_delegation(&existing);
    let original = if managed {
        String::new()
    } else {
        existing
            .replace(HOOK_SNIPPET, "")
            .replace(DELEGATION_SNIPPET, "")
    };
    let original_delegates = if managed {
        existing.contains(ORIGINAL_DELEGATION_MARKER)
    } else {
        has_delegation(&original)
    };
    let has_original = if managed {
        existing.contains(ORIGINAL_HOOK_SNIPPET)
    } else {
        // A shebang without a body needs no preserved executable.
        !original
            .strip_prefix("#!")
            .and_then(|script| script.split_once('\n').map(|(_, body)| body))
            .unwrap_or(&original)
            .trim()
            .is_empty()
    };

    // Moving a shell dispatcher changes $0 and can silently bypass its checks.
    // Refuse installation rather than change path-dependent hook behavior.
    anyhow::ensure!(
        managed || !(original.contains("$0") || original.contains("${0}")),
        "cannot wrap path-dependent hook {}; configure cibars signaling in that hook manually",
        path.display()
    );

    let mut content = String::from(HOOK_WRAPPER_HEADER);
    // Pre-push input is finite. A private file lets each hook read the complete ref list.
    content.push_str(
        "_cibars_stdin=$(mktemp \"${TMPDIR:-/tmp}/cibars-pre-push.XXXXXX\") || exit 1\n\
         trap 'rm -f \"$_cibars_stdin\"' 0\n\
         trap 'exit 1' HUP INT TERM\n\
         cat > \"$_cibars_stdin\" || exit $?\n",
    );
    if has_original {
        content.push_str(ORIGINAL_HOOK_SNIPPET);
    }
    // Assumption: custom delegation already owns its local-hook invocation. Only migrate
    // cibars' exact legacy block; never duplicate an unrelated hook manager's delegation.
    if original_delegates {
        content.push_str(ORIGINAL_DELEGATION_MARKER);
    } else if delegate {
        content.push_str(REPLAY_DELEGATION_SNIPPET);
    }
    if boost {
        content.push_str(HOOK_SNIPPET);
    }
    if existing == content {
        return Ok(false);
    }

    let permissions = match std::fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("cannot stat {}", path.display()));
        }
    };
    if !managed && has_original {
        let original_path = path.with_file_name("pre-push.cibars-original");
        let mut preserved = create_private_hook(&original_path).with_context(|| {
            format!(
                "cannot preserve original hook at {}",
                original_path.display()
            )
        })?;
        if let Some(permissions) = &permissions {
            preserved.set_permissions(permissions.clone())?;
        }
        preserved.write_all(original.as_bytes())?;
        make_hook_executable(&original_path)?;
    }
    // Replace the directory entry atomically: do not truncate a shared symlink target.
    let pending_path = path.with_file_name("pre-push.cibars-pending");
    let mut pending = create_private_hook(&pending_path)
        .with_context(|| format!("cannot stage hook at {}", pending_path.display()))?;
    let installed = (|| -> Result<()> {
        pending.write_all(content.as_bytes())?;
        if let Some(permissions) = permissions {
            pending.set_permissions(permissions)?;
        }
        make_hook_executable(&pending_path)?;
        std::fs::rename(&pending_path, path)?;
        Ok(())
    })();
    if installed.is_err() {
        if let Err(error) = std::fs::remove_file(&pending_path) {
            tracing::warn!(
                "cannot remove staged hook {}: {error}",
                pending_path.display()
            );
        }
    }
    installed.with_context(|| format!("cannot install hook at {}", path.display()))?;
    Ok(true)
}

fn has_cibars_hook(contents: &str) -> bool {
    contents.contains("USR1") && contents.contains("cibars")
}

fn needs_hook_migration(contents: &str) -> bool {
    if contents.starts_with(HOOK_WRAPPER_HEADER) || !contents.contains(HOOK_SNIPPET) {
        return false;
    }
    let original = contents
        .replace(HOOK_SNIPPET, "")
        .replace(DELEGATION_SNIPPET, "");
    !original
        .strip_prefix("#!")
        .map(|script| script.split_once('\n').map_or("", |(_, body)| body))
        .unwrap_or(&original)
        .trim()
        .is_empty()
}

fn has_delegation(contents: &str) -> bool {
    contents.contains(ORIGINAL_DELEGATION_MARKER)
        || (contents.contains("rev-parse --git-dir")
            && contents.contains("hooks/pre-push")
            && contents.contains("$_local"))
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
            if needs_hook_migration(&contents) {
                return HookStatus::Incomplete;
            }
            let location = if is_global {
                HookLocation::Global
            } else {
                HookLocation::Local
            };
            return HookStatus::Installed(location);
        }
        // Effective hook delegates to local which has cibars → installed
        if is_global && has_delegation(&contents) {
            if let Ok(local_contents) = std::fs::read_to_string(&local_hook) {
                if has_cibars_hook(&local_contents) {
                    if needs_hook_migration(&local_contents) {
                        return HookStatus::Incomplete;
                    }
                    return HookStatus::Installed(HookLocation::Global);
                }
            }
        }
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

/// Ensure global hook delegates to local hooks. Auto-fixes broken state where
/// both local and global pre-push hooks exist but global doesn't delegate.
/// Returns true if delegation was added.
pub fn ensure_delegation(dir: &Path) -> bool {
    let effective = effective_hooks_dir(dir);
    let local_hooks = dir.join(".git/hooks");
    if effective == local_hooks {
        return false; // No global override, nothing to fix
    }
    let global_hook = effective.join("pre-push");
    let local_hook = local_hooks.join("pre-push");
    if !global_hook.is_file() || !local_hook.is_file() {
        return false; // Both must exist for this to be a broken state
    }
    match install_hook_wrapper(&global_hook, false, true) {
        Ok(changed) => changed,
        Err(error) => {
            tracing::warn!("cannot repair global pre-push delegation: {error:#}");
            false
        }
    }
}

/// Returns true if core.hooksPath is set and differs from .git/hooks.
pub fn has_global_hooks_path(dir: &Path) -> bool {
    effective_hooks_dir(dir) != dir.join(".git/hooks")
}

pub fn install_pre_push_hook(dir: &Path, target: HookTarget) -> Result<()> {
    let hook_dir = match target {
        HookTarget::Local => dir.join(".git/hooks"),
        HookTarget::Global => effective_hooks_dir(dir),
    };
    let hook_path = hook_dir.join("pre-push");
    std::fs::create_dir_all(&hook_dir)
        .with_context(|| format!("cannot create {}", hook_dir.display()))?;

    install_hook_wrapper(&hook_path, true, target == HookTarget::Global)?;

    // Local install with global override: ensure global hook delegates to local
    if target == HookTarget::Local {
        let global_dir = effective_hooks_dir(dir);
        if global_dir != hook_dir {
            let global_hook_path = global_dir.join("pre-push");
            std::fs::create_dir_all(&global_dir)
                .with_context(|| format!("cannot create {}", global_dir.display()))?;
            install_hook_wrapper(&global_hook_path, false, true)?;
        }
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
        assert_eq!(config.aws.as_ref().unwrap().profile, "staging");
        assert_eq!(config.aws.as_ref().unwrap().region, "eu-west-1");
        assert_eq!(config.github.as_ref().unwrap().repo, "acme/backend");
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
            sources: None,
            branch: None,
            workflow_categories: None,
        };
        let config = Config::merge(&["cibars", "--aws-profile", "from-cli"], file).unwrap();
        assert_eq!(config.aws.as_ref().unwrap().profile, "from-cli");
        assert_eq!(config.aws.as_ref().unwrap().region, "eu-west-1");
        assert_eq!(config.github.as_ref().unwrap().repo, "org/repo");
    }

    #[test]
    fn merge_file_only_no_cli_args() {
        let file = FileConfig {
            aws_profile: Some("staging".into()),
            region: Some("eu-west-1".into()),
            github_repo: Some("acme/backend".into()),
            sources: None,
            branch: None,
            workflow_categories: None,
        };
        let config = Config::merge(&["cibars"], file).unwrap();
        assert_eq!(config.aws.as_ref().unwrap().profile, "staging");
        assert_eq!(config.aws.as_ref().unwrap().region, "eu-west-1");
        assert_eq!(config.github.as_ref().unwrap().repo, "acme/backend");
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
        assert_eq!(config.aws.as_ref().unwrap().profile, "p");
        assert_eq!(config.aws.as_ref().unwrap().region, "r");
        assert_eq!(config.github.as_ref().unwrap().repo, "o/r");
    }

    #[test]
    fn merge_missing_field_errors() {
        let file = FileConfig {
            aws_profile: Some("staging".into()),
            region: None,
            github_repo: None,
            sources: None,
            branch: None,
            workflow_categories: None,
        };
        let result = Config::merge(&["cibars"], file);
        assert!(result.is_err());
    }

    // --- optional sources ---

    fn src_file(sources: Option<Vec<&str>>) -> FileConfig {
        FileConfig {
            sources: sources.map(|v| v.into_iter().map(String::from).collect()),
            ..FileConfig::default()
        }
    }

    #[test]
    fn sources_default_to_both() {
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
        assert!(config.aws.is_some());
        assert!(config.github.is_some());
    }

    #[test]
    fn github_only_needs_no_aws_settings() {
        let config =
            Config::try_from_args(&["cibars", "--sources", "github", "--github-repo", "o/r"])
                .unwrap();
        assert!(config.aws.is_none());
        assert_eq!(config.github.unwrap().repo, "o/r");
    }

    #[test]
    fn aws_only_needs_no_github_settings() {
        let config = Config::try_from_args(&[
            "cibars",
            "--sources",
            "aws",
            "--aws-profile",
            "p",
            "--region",
            "r",
        ])
        .unwrap();
        assert!(config.github.is_none());
        let aws = config.aws.unwrap();
        assert_eq!((aws.profile.as_str(), aws.region.as_str()), ("p", "r"));
    }

    #[test]
    fn cli_sources_comma_separated() {
        let config = Config::try_from_args(&[
            "cibars",
            "--sources",
            "aws,github",
            "--aws-profile",
            "p",
            "--region",
            "r",
            "--github-repo",
            "o/r",
        ])
        .unwrap();
        assert!(config.aws.is_some() && config.github.is_some());
    }

    #[test]
    fn file_sources_respected() {
        let mut file = src_file(Some(vec!["github"]));
        file.github_repo = Some("o/r".into());
        let config = Config::merge(&["cibars"], file).unwrap();
        assert!(config.aws.is_none());
        assert!(config.github.is_some());
    }

    #[test]
    fn cli_sources_override_file() {
        let mut file = src_file(Some(vec!["github"]));
        file.aws_profile = Some("p".into());
        file.region = Some("r".into());
        let config = Config::merge(&["cibars", "--sources", "aws"], file).unwrap();
        assert!(config.github.is_none());
        assert!(config.aws.is_some());
    }

    #[test]
    fn github_enabled_without_repo_fails() {
        let err = Config::try_from_args(&["cibars", "--sources", "github"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("github_repo"), "got: {err}");
    }

    #[test]
    fn aws_enabled_without_region_fails() {
        let err = Config::try_from_args(&["cibars", "--sources", "aws", "--aws-profile", "p"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("region"), "got: {err}");
    }

    #[test]
    fn empty_sources_fails() {
        let err = Config::merge(&["cibars"], src_file(Some(vec![])))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sources"), "got: {err}");
    }

    #[test]
    fn unknown_source_fails() {
        let err = Config::merge(&["cibars"], src_file(Some(vec!["gitlab"])))
            .unwrap_err()
            .to_string();
        assert!(err.contains("gitlab"), "got: {err}");
    }

    #[test]
    fn file_config_parses_sources_key() {
        let fc: FileConfig = toml::from_str("sources = [\"aws\"]").unwrap();
        assert_eq!(fc.sources, Some(vec!["aws".to_string()]));
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
    fn hook_snippet_uses_git_toplevel() {
        assert!(
            HOOK_SNIPPET.contains("$(git rev-parse --show-toplevel | tr '/' '_')"),
            "snippet should use git rev-parse --show-toplevel, got: {HOOK_SNIPPET}"
        );
    }

    #[test]
    fn hook_snippet_does_not_use_bare_pwd() {
        assert!(
            !HOOK_SNIPPET.contains("$(pwd"),
            "snippet must not use pwd (breaks after cd), got: {HOOK_SNIPPET}"
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
        // Global with delegation is just Global (always delegates)
        assert_eq!(
            check_pre_push_hook(dir.path()),
            HookStatus::Installed(HookLocation::Global)
        );
    }

    #[test]
    fn check_hook_delegation_to_local_reports_global() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // Global hook has only delegation, no cibars snippet
        std::fs::write(
            global_hooks.join("pre-push"),
            format!("#!/bin/sh{DELEGATION_SNIPPET}"),
        )
        .unwrap();
        // Local hook has the cibars snippet
        let local_hooks = dir.path().join(".git/hooks");
        std::fs::write(
            local_hooks.join("pre-push"),
            format!("#!/bin/sh{HOOK_SNIPPET}"),
        )
        .unwrap();
        // Should report Global since global hook is the entry point
        assert_eq!(
            check_pre_push_hook(dir.path()),
            HookStatus::Installed(HookLocation::Global)
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

    #[cfg(unix)]
    fn write_executable_hook(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, contents).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn run_installed_hook(repo: &Path, path: &Path) -> std::process::Output {
        use std::io::Seek;
        let mut input = tempfile::tempfile().unwrap();
        input
            .write_all(b"refs/heads/main abc refs/heads/main def\n")
            .unwrap();
        input.rewind().unwrap();
        Command::new(path)
            .args(["origin", "ssh://example.test/repo with spaces"])
            .current_dir(repo)
            .env("HOME", repo)
            .stdin(input)
            .output()
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn installed_local_hook_preserves_original_failure() {
        for original in ["#!/bin/sh\nfalse\n", "#!/bin/sh\nexit 23\n"] {
            let dir = tempfile::tempdir().unwrap();
            init_git_repo(dir.path());
            let hook = dir.path().join(".git/hooks/pre-push");
            write_executable_hook(&hook, original);
            let expected = run_installed_hook(dir.path(), &hook).status.code();
            install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
            assert_eq!(
                run_installed_hook(dir.path(), &hook).status.code(),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn installed_local_hook_signals_after_original_exit_or_exec() {
        use std::os::unix::process::ExitStatusExt;
        use std::time::{Duration, Instant};
        for original in ["#!/bin/sh\nexit 0\n", "#!/bin/sh\nexec true\n"] {
            let dir = tempfile::tempdir().unwrap();
            init_git_repo(dir.path());
            let hook = dir.path().join(".git/hooks/pre-push");
            write_executable_hook(&hook, original);
            install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();

            // A separate process keeps SIGUSR1 assertions isolated from other tests.
            let mut receiver = Command::new("sleep").arg("30").spawn().unwrap();
            let pids = dir.path().join(".cibars/pids");
            std::fs::create_dir_all(&pids).unwrap();
            let canonical = dir.path().canonicalize().unwrap();
            let name = canonical.to_string_lossy().replace('/', "_");
            std::fs::write(pids.join(format!("{name}.pid")), receiver.id().to_string()).unwrap();
            let output = run_installed_hook(dir.path(), &hook);
            let deadline = Instant::now() + Duration::from_secs(2);
            let received = loop {
                if let Some(status) = receiver.try_wait().unwrap() {
                    break status.signal() == Some(signal_hook::consts::SIGUSR1);
                }
                if Instant::now() >= deadline {
                    receiver.kill().unwrap();
                    receiver.wait().unwrap();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(output.status.success(), "{output:?}");
            assert!(received, "original hook prevented boost: {original}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn installed_hook_preserves_interpreter_arguments_and_stdin() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hook = dir.path().join(".git/hooks/pre-push");
        write_executable_hook(
            &hook,
            "#!/bin/bash\nargs=(\"$@\")\nprintf '%s\\n' \"${args[@]}\"\ncat\n",
        );
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        let output = run_installed_hook(dir.path(), &hook);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "origin\nssh://example.test/repo with spaces\nrefs/heads/main abc refs/heads/main def\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_global_hook_replays_stdin_to_both_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        let global = global_hooks.join("pre-push");
        write_executable_hook(&global, "#!/bin/sh\nprintf 'global\\n'\ncat\n");
        write_executable_hook(
            &dir.path().join(".git/hooks/pre-push"),
            "#!/bin/sh\nprintf 'local\\n'\ncat\n",
        );
        install_pre_push_hook(dir.path(), HookTarget::Global).unwrap();
        let output = run_installed_hook(dir.path(), &global);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "global\nrefs/heads/main abc refs/heads/main def\nlocal\nrefs/heads/main abc refs/heads/main def\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn global_delegation_preserves_failures_and_handles_original_exit() {
        for target in [Some(HookTarget::Global), Some(HookTarget::Local), None] {
            for (global_body, local_body, expected, stdout) in [
                ("false", "printf local", 1, ""),
                ("exit 0", "printf local", 0, "local"),
                ("exec true", "printf local", 0, "local"),
                ("true", "exit 31", 31, ""),
                (
                    "printf 'global\\n'; cat",
                    "printf 'local\\n'; cat",
                    0,
                    "global\nrefs/heads/main abc refs/heads/main def\nlocal\nrefs/heads/main abc refs/heads/main def\n",
                ),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let global_hooks = dir.path().join("global-hooks");
                std::fs::create_dir_all(&global_hooks).unwrap();
                init_git_repo_with_hooks_path(dir.path(), &global_hooks);
                let global = global_hooks.join("pre-push");
                write_executable_hook(&global, &format!("#!/bin/sh\n{global_body}\n"));
                write_executable_hook(
                    &dir.path().join(".git/hooks/pre-push"),
                    &format!("#!/bin/sh\n{local_body}\n"),
                );
                if let Some(target) = target {
                    install_pre_push_hook(dir.path(), target).unwrap();
                } else {
                    assert!(ensure_delegation(dir.path()));
                    assert!(!ensure_delegation(dir.path()), "repair must be idempotent");
                }
                let output = run_installed_hook(dir.path(), &global);
                assert_eq!(output.status.code(), Some(expected), "{target:?}: {output:?}");
                assert_eq!(String::from_utf8(output.stdout).unwrap(), stdout);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn installed_global_hook_does_not_delegate_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hook = dir.path().join(".git/hooks/pre-push");
        // Bound a broken recursive delegation without creating an unbounded process tree.
        write_executable_hook(
            &hook,
            "#!/bin/sh\n[ ! -e hook-ran ] || exit 73\ntouch hook-ran\n",
        );
        install_pre_push_hook(dir.path(), HookTarget::Global).unwrap();
        let output = run_installed_hook(dir.path(), &hook);
        assert!(output.status.success(), "{output:?}");
    }

    #[cfg(unix)]
    #[test]
    fn installed_hook_runs_original_once_after_reinstallation() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hook = dir.path().join(".git/hooks/pre-push");
        write_executable_hook(&hook, "#!/bin/sh\nprintf original\n");
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        let output = run_installed_hook(dir.path(), &hook);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"original");
    }

    #[cfg(unix)]
    #[test]
    fn installed_hook_repairs_legacy_appended_snippet() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hook = dir.path().join(".git/hooks/pre-push");
        write_executable_hook(&hook, &format!("#!/bin/sh\nfalse\n{HOOK_SNIPPET}"));
        assert_eq!(check_pre_push_hook(dir.path()), HookStatus::Incomplete);
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        assert_eq!(run_installed_hook(dir.path(), &hook).status.code(), Some(1));
        assert_eq!(
            check_pre_push_hook(dir.path()),
            HookStatus::Installed(HookLocation::Local)
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_hook_refuses_path_dependent_original() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hook = dir.path().join(".git/hooks/pre-push");
        let body = "#!/bin/sh\ncase \"$0\" in */pre-push) exit 23;; *) exit 0;; esac\n";
        write_executable_hook(&hook, body);
        assert_eq!(
            run_installed_hook(dir.path(), &hook).status.code(),
            Some(23)
        );
        assert!(install_pre_push_hook(dir.path(), HookTarget::Local).is_err());
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), body);
    }

    #[cfg(unix)]
    #[test]
    fn installed_hook_refuses_to_overwrite_preserved_original() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hook = dir.path().join(".git/hooks/pre-push");
        let original = dir.path().join(".git/hooks/pre-push.cibars-original");
        write_executable_hook(&hook, "#!/bin/sh\nfalse\n");
        std::fs::write(&original, "existing backup").unwrap();
        assert!(install_pre_push_hook(dir.path(), HookTarget::Local).is_err());
        assert_eq!(
            std::fs::read_to_string(original).unwrap(),
            "existing backup"
        );
        assert_eq!(run_installed_hook(dir.path(), &hook).status.code(), Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn installed_hook_keeps_custom_delegation_once_after_reinstallation() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        let global = global_hooks.join("pre-push");
        write_executable_hook(
            &global,
            "#!/bin/sh\n_local=\"$(git rev-parse --git-dir)/hooks/pre-push\"\nexec \"$_local\" \"$@\"\n",
        );
        write_executable_hook(
            &dir.path().join(".git/hooks/pre-push"),
            "#!/bin/sh\nprintf local\n",
        );
        install_pre_push_hook(dir.path(), HookTarget::Global).unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Global).unwrap();
        assert!(!ensure_delegation(dir.path()));
        let output = run_installed_hook(dir.path(), &global);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"local");
    }

    #[cfg(unix)]
    #[test]
    fn installed_hook_preserves_private_hook_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hook = dir.path().join(".git/hooks/pre-push");
        write_executable_hook(&hook, "#!/bin/sh\nexit 0\n");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        for path in [hook, dir.path().join(".git/hooks/pre-push.cibars-original")] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn installed_hook_does_not_overwrite_symlink_target() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hook = dir.path().join(".git/hooks/pre-push");
        let shared = dir.path().join("shared-hook");
        write_executable_hook(&shared, "#!/bin/sh\nprintf shared\n");
        symlink(&shared, &hook).unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        assert_eq!(
            std::fs::read_to_string(shared).unwrap(),
            "#!/bin/sh\nprintf shared\n"
        );
        let output = run_installed_hook(dir.path(), &hook);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"shared");
    }

    #[test]
    fn install_hook_local_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert!(content.contains("#!/bin/sh"));
        assert!(content.contains("cibars"));
        assert!(content.contains("$(git rev-parse --show-toplevel | tr"));
    }

    #[cfg(unix)]
    #[test]
    fn install_hook_local_preserves_existing_behavior() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let hooks_dir = dir.path().join(".git/hooks");
        std::fs::write(hooks_dir.join("pre-push"), "#!/bin/sh\necho pushing\n").unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert!(content.contains("cibars"));
        let output = run_installed_hook(dir.path(), &hooks_dir.join("pre-push"));
        assert!(output.status.success());
        assert_eq!(output.stdout, b"pushing\n");
    }

    #[test]
    fn install_hook_local_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert_eq!(
            content.matches("cibars: boost").count(),
            1,
            "should not duplicate"
        );
    }

    #[test]
    fn install_hook_local_no_delegation() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        let content = std::fs::read_to_string(hooks_dir.join("pre-push")).unwrap();
        assert!(
            !has_delegation(&content),
            "local install should not add delegation: {content}"
        );
    }

    #[test]
    fn install_hook_global_in_global_dir() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        install_pre_push_hook(dir.path(), HookTarget::Global).unwrap();
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(
            content.contains("cibars"),
            "snippet should be in global hooks"
        );
        assert!(
            has_delegation(&content),
            "global always delegates: {content}"
        );
    }

    #[test]
    fn install_hook_global_adds_delegation_when_local_hook_exists() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        let local_hooks = dir.path().join(".git/hooks");
        std::fs::write(
            local_hooks.join("pre-push"),
            "#!/bin/sh\ncd backend && pytest\n",
        )
        .unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Global).unwrap();
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(has_delegation(&content), "should add delegation: {content}");
        assert!(
            has_cibars_hook(&content),
            "should add cibars snippet: {content}"
        );
    }

    #[test]
    fn install_hook_global_skips_delegation_when_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // Global hook already has delegation
        std::fs::write(
            global_hooks.join("pre-push"),
            format!("#!/bin/sh{DELEGATION_SNIPPET}"),
        )
        .unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Global).unwrap();
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert_eq!(
            content.matches("rev-parse --git-dir").count(),
            1,
            "should not duplicate delegation: {content}"
        );
    }

    #[test]
    fn install_hook_global_always_delegates_even_without_local_hook() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // No local hook — delegation should still be added (safe no-op)
        install_pre_push_hook(dir.path(), HookTarget::Global).unwrap();
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(
            has_delegation(&content),
            "global always delegates: {content}"
        );
        assert!(has_cibars_hook(&content));
    }

    #[cfg(unix)]
    #[test]
    fn install_hook_sets_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        let hooks_dir = dir.path().join(".git/hooks");
        let perms = std::fs::metadata(hooks_dir.join("pre-push"))
            .unwrap()
            .permissions();
        assert!(perms.mode() & 0o111 != 0, "hook should be executable");
    }

    #[test]
    fn install_local_creates_global_delegation_when_global_override_active() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // No pre-push in global dir, nothing in local — mirrors the reported bug
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        // Local hook should have cibars snippet
        let local = std::fs::read_to_string(dir.path().join(".git/hooks/pre-push")).unwrap();
        assert!(has_cibars_hook(&local), "local should have cibars: {local}");
        // Global hook should have delegation (so local hook actually runs)
        let global = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(
            has_delegation(&global),
            "global should delegate to local: {global}"
        );
        // Status should be Installed, not Shadowed
        let status = check_pre_push_hook(dir.path());
        assert!(
            matches!(status, HookStatus::Installed(_)),
            "expected Installed, got {status:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_local_preserves_existing_global_hook_and_adds_delegation() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // Global hook has existing content but no delegation
        std::fs::write(global_hooks.join("pre-push"), "#!/bin/sh\necho existing\n").unwrap();
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        let global = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(has_delegation(&global), "should add delegation: {global}");
        let output = run_installed_hook(dir.path(), &global_hooks.join("pre-push"));
        assert!(output.status.success());
        assert_eq!(output.stdout, b"existing\n");
    }

    #[test]
    fn install_local_no_global_delegation_without_override() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        install_pre_push_hook(dir.path(), HookTarget::Local).unwrap();
        // Only local hook should exist
        let local = std::fs::read_to_string(dir.path().join(".git/hooks/pre-push")).unwrap();
        assert!(has_cibars_hook(&local));
        // No delegation in local hook (no global override)
        assert!(!has_delegation(&local), "no delegation needed: {local}");
    }

    // --- has_global_hooks_path tests ---

    // --- ensure_delegation tests ---

    #[test]
    fn ensure_delegation_noop_without_global_override() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        assert!(!ensure_delegation(dir.path()));
    }

    #[test]
    fn ensure_delegation_noop_when_no_local_hook() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        std::fs::write(
            global_hooks.join("pre-push"),
            format!("#!/bin/sh{HOOK_SNIPPET}"),
        )
        .unwrap();
        assert!(!ensure_delegation(dir.path()));
    }

    #[test]
    fn ensure_delegation_fixes_broken_state() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        // Both hooks exist but global doesn't delegate
        std::fs::write(
            global_hooks.join("pre-push"),
            format!("#!/bin/sh{HOOK_SNIPPET}"),
        )
        .unwrap();
        let local_hooks = dir.path().join(".git/hooks");
        std::fs::write(local_hooks.join("pre-push"), "#!/bin/sh\necho local\n").unwrap();
        assert!(ensure_delegation(dir.path()));
        let content = std::fs::read_to_string(global_hooks.join("pre-push")).unwrap();
        assert!(has_delegation(&content), "should add delegation: {content}");
    }

    #[test]
    fn ensure_delegation_migrates_legacy_delegation_once() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        std::fs::write(
            global_hooks.join("pre-push"),
            format!("#!/bin/sh{DELEGATION_SNIPPET}{HOOK_SNIPPET}"),
        )
        .unwrap();
        let local_hooks = dir.path().join(".git/hooks");
        std::fs::write(local_hooks.join("pre-push"), "#!/bin/sh\necho local\n").unwrap();
        assert!(ensure_delegation(dir.path()));
        assert!(!ensure_delegation(dir.path()));
    }

    #[test]
    fn has_global_hooks_path_false_by_default() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        assert!(!has_global_hooks_path(dir.path()));
    }

    #[test]
    fn has_global_hooks_path_true_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let global_hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&global_hooks).unwrap();
        init_git_repo_with_hooks_path(dir.path(), &global_hooks);
        assert!(has_global_hooks_path(dir.path()));
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
            sources: None,
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
            sources: None,
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
        assert_eq!(
            config.github.as_ref().unwrap().branch,
            Some("master".to_string())
        );
    }

    #[test]
    fn branch_from_file_config() {
        let file = FileConfig {
            aws_profile: Some("p".into()),
            region: Some("r".into()),
            github_repo: Some("o/r".into()),
            sources: None,
            branch: Some("master".into()),
            workflow_categories: None,
        };
        let config = Config::merge(&["cibars"], file).unwrap();
        assert_eq!(
            config.github.as_ref().unwrap().branch,
            Some("master".to_string())
        );
    }

    #[test]
    fn branch_cli_overrides_file() {
        let file = FileConfig {
            aws_profile: Some("p".into()),
            region: Some("r".into()),
            github_repo: Some("o/r".into()),
            sources: None,
            branch: Some("develop".into()),
            workflow_categories: None,
        };
        let config = Config::merge(&["cibars", "--branch", "main"], file).unwrap();
        assert_eq!(
            config.github.as_ref().unwrap().branch,
            Some("main".to_string())
        );
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
        assert!(config.github.as_ref().unwrap().branch.is_none());
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
