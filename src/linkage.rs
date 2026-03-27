use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::model::BuildStatus;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CachedLink {
    pub pipeline_name: String,
    pub workflow_name: String,
    pub s3_bucket: String,
    pub s3_key: String,
    pub source: CachedLinkSource,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum CachedLinkSource {
    YamlDiscovered,
    RuntimeCorrelated,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LinkCache {
    pub schema_version: u32,
    pub discovered_at: String,
    pub links: Vec<CachedLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkSource {
    YamlDiscovered,
    RuntimeCorrelated,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PipelineLink {
    pub pipeline_name: String,
    pub workflow_name: String,
    pub s3_bucket: String,
    pub s3_key: String,
    pub source: LinkSource,
}

pub struct LinkMap {
    links: Vec<PipelineLink>,
    /// Track recent GH workflow completions for runtime correlation.
    recent_completions: Vec<(String, Instant)>,
}

/// Correlation window: a CP pipeline must start within this duration
/// of a GH workflow completion to be considered linked.
const CORRELATION_WINDOW_SECS: u64 = 30;

impl LinkMap {
    pub fn new() -> Self {
        Self {
            links: Vec::new(),
            recent_completions: Vec::new(),
        }
    }

    /// Add a link discovered from YAML/config parsing.
    pub fn add_discovered(
        &mut self,
        pipeline: String,
        workflow: String,
        bucket: String,
        key: String,
    ) {
        // Avoid duplicates
        if self
            .links
            .iter()
            .any(|l| l.pipeline_name == pipeline && l.workflow_name == workflow)
        {
            return;
        }
        tracing::info!(
            pipeline = %pipeline,
            workflow = %workflow,
            bucket = %bucket,
            key = %key,
            "discovered GH→CP link"
        );
        self.links.push(PipelineLink {
            pipeline_name: pipeline,
            workflow_name: workflow,
            s3_bucket: bucket,
            s3_key: key,
            source: LinkSource::YamlDiscovered,
        });
    }

    /// Find the linked GH workflow name for a given CP pipeline.
    pub fn workflow_for_pipeline(&self, pipeline_name: &str) -> Option<&str> {
        self.links
            .iter()
            .find(|l| l.pipeline_name == pipeline_name)
            .map(|l| l.workflow_name.as_str())
    }

    /// Record a GH workflow completion for runtime correlation.
    pub fn record_workflow_completion(&mut self, workflow_name: &str) {
        self.recent_completions
            .push((workflow_name.to_string(), Instant::now()));
        self.prune_expired();
    }

    /// Try to correlate a newly-started CP pipeline with a recently-completed
    /// GH workflow. Returns the workflow name if a correlation is found.
    pub fn try_correlate(&mut self, pipeline_name: &str) -> Option<String> {
        self.prune_expired();

        // If we already have a YAML-discovered link, validate it
        if let Some(link) = self.links.iter().find(|l| l.pipeline_name == pipeline_name) {
            let wf = &link.workflow_name;
            if self.recent_completions.iter().any(|(n, _)| n == wf) {
                tracing::info!(
                    pipeline = %pipeline_name,
                    workflow = %wf,
                    "runtime validated YAML-discovered link"
                );
                return Some(wf.clone());
            }
            return None;
        }

        // No YAML link — try to discover one from timing
        if let Some((wf_name, _)) = self.recent_completions.last() {
            let wf_name = wf_name.clone();
            tracing::info!(
                pipeline = %pipeline_name,
                workflow = %wf_name,
                "runtime-correlated new GH→CP link"
            );
            self.links.push(PipelineLink {
                pipeline_name: pipeline_name.to_string(),
                workflow_name: wf_name.clone(),
                s3_bucket: String::new(),
                s3_key: String::new(),
                source: LinkSource::RuntimeCorrelated,
            });
            return Some(wf_name);
        }

        None
    }

    /// All current links (for logging/debugging).
    pub fn links(&self) -> &[PipelineLink] {
        &self.links
    }

    /// Populate from a loaded cache (replaces any existing links).
    pub fn load_from_cache(&mut self, cache: LinkCache) {
        self.links.clear();
        for cl in cache.links {
            self.links.push(PipelineLink {
                pipeline_name: cl.pipeline_name,
                workflow_name: cl.workflow_name,
                s3_bucket: cl.s3_bucket,
                s3_key: cl.s3_key,
                source: match cl.source {
                    CachedLinkSource::YamlDiscovered => LinkSource::YamlDiscovered,
                    CachedLinkSource::RuntimeCorrelated => LinkSource::RuntimeCorrelated,
                },
            });
        }
    }

    /// Serialize current links to a cache struct.
    pub fn to_cache(&self, discovered_at: &str) -> LinkCache {
        LinkCache {
            schema_version: 1,
            discovered_at: discovered_at.to_string(),
            links: self
                .links
                .iter()
                .map(|l| CachedLink {
                    pipeline_name: l.pipeline_name.clone(),
                    workflow_name: l.workflow_name.clone(),
                    s3_bucket: l.s3_bucket.clone(),
                    s3_key: l.s3_key.clone(),
                    source: match l.source {
                        LinkSource::YamlDiscovered => CachedLinkSource::YamlDiscovered,
                        LinkSource::RuntimeCorrelated => CachedLinkSource::RuntimeCorrelated,
                    },
                })
                .collect(),
        }
    }

    /// Remove all links and recent completions (before re-discovery).
    pub fn clear(&mut self) {
        self.links.clear();
        self.recent_completions.clear();
    }

    fn prune_expired(&mut self) {
        let cutoff = std::time::Duration::from_secs(CORRELATION_WINDOW_SECS);
        self.recent_completions
            .retain(|(_, t)| t.elapsed() < cutoff);
    }
}

/// Load link cache from disk. Returns None if file doesn't exist or
/// has an unknown schema version.
pub fn load_link_cache(path: &std::path::Path) -> anyhow::Result<Option<LinkCache>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let cache: LinkCache =
                toml::from_str(&contents).context("failed to parse .cibars-links.toml")?;
            if cache.schema_version != 1 {
                return Ok(None);
            }
            Ok(Some(cache))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write link cache atomically (write to .tmp, then rename).
pub fn save_link_cache(path: &std::path::Path, cache: &LinkCache) -> anyhow::Result<()> {
    let body = toml::to_string_pretty(cache).context("failed to serialize link cache")?;
    let content = format!("# Auto-generated by cibars. Re-run discovery with 'l'.\n\n{body}");
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, content.as_bytes()).context("failed to write .cibars-links.toml.tmp")?;
    std::fs::rename(&tmp, path).context("failed to rename link cache into place")?;
    Ok(())
}

/// Match S3 keys: exact match or prefix match (CP key is prefix of GH upload key).
/// Ignores leading slashes for normalization.
pub fn s3_keys_match(cp_key: &str, gh_key: &str) -> bool {
    let cp = cp_key.trim_start_matches('/');
    let gh = gh_key.trim_start_matches('/');

    if cp.is_empty() || gh.is_empty() {
        return false;
    }

    cp == gh || gh.starts_with(cp) || cp.starts_with(gh)
}

/// After polling, apply GH→CP links:
/// 1. Record GH workflow completions for runtime correlation
/// 2. When a CP pipeline starts Running, mark linked GH workflow as Succeeded
///
/// `stopped_runs` tracks workflow_name → run_id that was already suppressed,
/// so new GH runs triggered while CP is still running are not suppressed.
pub fn apply_links(
    app: &Arc<Mutex<App>>,
    link_map: &mut LinkMap,
    stopped_runs: &mut HashMap<String, u64>,
) {
    let a = app.lock().expect("app mutex poisoned");

    // Record GH workflow completions (non-Running terminal states)
    for wg in &a.workflow_groups {
        if wg.summary_status == BuildStatus::Succeeded || wg.summary_status == BuildStatus::Failed {
            link_map.record_workflow_completion(&wg.name);
        }
    }

    // Collect pipelines that are Running and have a linked workflow
    let running_links: Vec<(String, String)> = a
        .pipeline_groups
        .iter()
        .filter(|pg| pg.summary_status == BuildStatus::Running)
        .filter_map(|pg| {
            link_map
                .workflow_for_pipeline(&pg.name)
                .map(|wf| (pg.name.clone(), wf.to_string()))
        })
        .collect();

    // Also try runtime correlation for Running pipelines without YAML links
    let unlinked_running: Vec<String> = a
        .pipeline_groups
        .iter()
        .filter(|pg| pg.summary_status == BuildStatus::Running)
        .filter(|pg| link_map.workflow_for_pipeline(&pg.name).is_none())
        .map(|pg| pg.name.clone())
        .collect();

    // Collect linked workflow names whose CP is no longer Running
    let linked_wf_names: Vec<String> = link_map
        .links()
        .iter()
        .filter(|l| {
            !a.pipeline_groups
                .iter()
                .any(|pg| pg.name == l.pipeline_name && pg.summary_status == BuildStatus::Running)
        })
        .map(|l| l.workflow_name.clone())
        .collect();

    drop(a);

    // Clear stopped_runs for workflows whose linked CP is no longer Running
    for wf_name in &linked_wf_names {
        stopped_runs.remove(wf_name);
    }

    let mut correlated_links = Vec::new();
    for pipe_name in &unlinked_running {
        if let Some(wf_name) = link_map.try_correlate(pipe_name) {
            correlated_links.push((pipe_name.clone(), wf_name));
        }
    }

    let all_links: Vec<(String, String)> =
        running_links.into_iter().chain(correlated_links).collect();

    if all_links.is_empty() {
        // Still need to set pending_link for linked pipelines
        let mut a = app.lock().expect("app mutex poisoned");
        set_pending_links(&mut a, link_map);
        return;
    }

    let mut a = app.lock().expect("app mutex poisoned");
    for (_pipe_name, wf_name) in &all_links {
        if let Some(wg) = a.workflow_groups.iter_mut().find(|g| g.name == *wf_name) {
            if wg.summary_status == BuildStatus::Running {
                if let Some(current_run_id) = wg.run_id {
                    // Already stopped this exact run — no-op
                    if stopped_runs.get(wf_name.as_str()) == Some(&current_run_id) {
                        continue;
                    }
                    // Different run from what we stopped — new trigger, don't suppress
                    if stopped_runs.contains_key(wf_name.as_str()) {
                        continue;
                    }
                    // First time seeing this workflow Running while CP runs → stop it
                    stopped_runs.insert(wf_name.clone(), current_run_id);
                }
                tracing::info!(
                    workflow = %wf_name,
                    run_id = ?wg.run_id,
                    "marking GH workflow as Succeeded (linked CP running)"
                );
                wg.summary_status = BuildStatus::Succeeded;
                for job in &mut wg.jobs {
                    if job.status == BuildStatus::Running {
                        job.set_status(BuildStatus::Succeeded);
                    }
                }
            }
        }
    }

    set_pending_links(&mut a, link_map);
}

/// Set `pending_link` on pipeline groups based on linked workflow state.
fn set_pending_links(app: &mut App, link_map: &LinkMap) {
    for pg in &mut app.pipeline_groups {
        if let Some(wf_name) = link_map.workflow_for_pipeline(&pg.name) {
            let linked_wf_running = app
                .workflow_groups
                .iter()
                .any(|wg| wg.name == wf_name && wg.summary_status == BuildStatus::Running);
            pg.pending_link = linked_wf_running && pg.summary_status != BuildStatus::Running;
        } else {
            pg.pending_link = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bar, PipelineGroup, WorkflowCategory, WorkflowGroup};
    use std::collections::HashMap;
    use std::time::Duration;

    // --- load/save link cache tests ---

    #[test]
    fn save_and_load_link_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cibars-links.toml");

        let mut map = LinkMap::new();
        map.add_discovered(
            "deploy".into(),
            "CI".into(),
            "bucket".into(),
            "art.zip".into(),
        );
        let cache = map.to_cache("2026-03-27T12:00:00Z");

        save_link_cache(&path, &cache).unwrap();

        let loaded = load_link_cache(&path).unwrap().unwrap();
        assert_eq!(loaded.schema_version, 1);
        assert_eq!(loaded.discovered_at, "2026-03-27T12:00:00Z");
        assert_eq!(loaded.links.len(), 1);
        assert_eq!(loaded.links[0].pipeline_name, "deploy");
    }

    #[test]
    fn load_link_cache_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let result = load_link_cache(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_link_cache_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cibars-links.toml");
        std::fs::write(&path, "not valid toml {{{{").unwrap();
        let result = load_link_cache(&path);
        assert!(result.is_err());
    }

    #[test]
    fn load_link_cache_unknown_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cibars-links.toml");
        let content = "schema_version = 99\ndiscovered_at = \"x\"\nlinks = []\n";
        std::fs::write(&path, content).unwrap();
        let result = load_link_cache(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn save_link_cache_includes_header_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cibars-links.toml");
        let cache = LinkCache {
            schema_version: 1,
            discovered_at: "now".into(),
            links: vec![],
        };
        save_link_cache(&path, &cache).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("# Auto-generated by cibars"));
    }

    // --- s3_keys_match tests ---

    #[test]
    fn s3_keys_exact_match() {
        assert!(s3_keys_match("my-app/artifact.zip", "my-app/artifact.zip"));
    }

    #[test]
    fn s3_keys_prefix_match_cp_prefix_of_gh() {
        assert!(s3_keys_match("my-app/", "my-app/artifact.zip"));
    }

    #[test]
    fn s3_keys_prefix_match_gh_prefix_of_cp() {
        assert!(s3_keys_match("my-app/artifact.zip", "my-app/"));
    }

    #[test]
    fn s3_keys_leading_slash_normalization() {
        assert!(s3_keys_match("/my-app/artifact.zip", "my-app/artifact.zip"));
    }

    #[test]
    fn s3_keys_no_match() {
        assert!(!s3_keys_match("frontend/build.zip", "backend/build.zip"));
    }

    #[test]
    fn s3_keys_empty_cp() {
        assert!(!s3_keys_match("", "my-app/artifact.zip"));
    }

    #[test]
    fn s3_keys_empty_gh() {
        assert!(!s3_keys_match("my-app/artifact.zip", ""));
    }

    #[test]
    fn s3_keys_both_empty() {
        assert!(!s3_keys_match("", ""));
    }

    // --- LinkMap tests ---

    #[test]
    fn add_discovered_and_lookup() {
        let mut map = LinkMap::new();
        map.add_discovered(
            "my-pipeline".into(),
            "CI".into(),
            "bucket".into(),
            "key".into(),
        );
        assert_eq!(map.workflow_for_pipeline("my-pipeline"), Some("CI"));
        assert_eq!(map.workflow_for_pipeline("other-pipeline"), None);
    }

    #[test]
    fn add_discovered_deduplicates() {
        let mut map = LinkMap::new();
        map.add_discovered("pipe".into(), "CI".into(), "b".into(), "k".into());
        map.add_discovered("pipe".into(), "CI".into(), "b".into(), "k".into());
        assert_eq!(map.links().len(), 1);
    }

    #[test]
    fn try_correlate_no_completions() {
        let mut map = LinkMap::new();
        assert_eq!(map.try_correlate("pipe"), None);
    }

    #[test]
    fn try_correlate_with_recent_completion() {
        let mut map = LinkMap::new();
        map.record_workflow_completion("CI");
        let result = map.try_correlate("deploy-pipe");
        assert_eq!(result, Some("CI".to_string()));
        // Should have added a runtime-correlated link
        assert_eq!(map.links().len(), 1);
        assert_eq!(map.links()[0].source, LinkSource::RuntimeCorrelated);
    }

    #[test]
    fn try_correlate_validates_yaml_link() {
        let mut map = LinkMap::new();
        map.add_discovered("pipe".into(), "CI".into(), "b".into(), "k".into());
        map.record_workflow_completion("CI");
        let result = map.try_correlate("pipe");
        assert_eq!(result, Some("CI".to_string()));
        // No new link added (already had YAML one)
        assert_eq!(map.links().len(), 1);
    }

    #[test]
    fn try_correlate_yaml_link_no_recent_completion() {
        let mut map = LinkMap::new();
        map.add_discovered("pipe".into(), "CI".into(), "b".into(), "k".into());
        // No completion recorded
        let result = map.try_correlate("pipe");
        assert_eq!(result, None);
    }

    #[test]
    fn correlation_expires_after_window() {
        let mut map = LinkMap::new();
        // Record completion, then simulate time passing beyond window.
        // We can't easily fake Instant, so test the prune logic indirectly
        // by checking that an empty recent_completions returns None.
        // The actual expiry is tested via the 30s window in production.
        map.recent_completions
            .push(("CI".to_string(), Instant::now() - Duration::from_secs(60)));
        let result = map.try_correlate("pipe");
        assert_eq!(result, None);
    }

    #[test]
    fn multiple_links_different_pipelines() {
        let mut map = LinkMap::new();
        map.add_discovered(
            "frontend-pipe".into(),
            "Frontend CI".into(),
            "b".into(),
            "fe/".into(),
        );
        map.add_discovered(
            "backend-pipe".into(),
            "Backend CI".into(),
            "b".into(),
            "be/".into(),
        );
        assert_eq!(
            map.workflow_for_pipeline("frontend-pipe"),
            Some("Frontend CI")
        );
        assert_eq!(
            map.workflow_for_pipeline("backend-pipe"),
            Some("Backend CI")
        );
    }

    // --- apply_links integration tests ---

    fn make_app_with_link_scenario() -> (Arc<Mutex<App>>, LinkMap) {
        let mut app = App::new();
        // GH workflow still Running
        let mut job = Bar::new("build".into());
        job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        // CP pipeline just started Running
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        });

        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "deploy-pipe".into(),
            "CI".into(),
            "bucket".into(),
            "key".into(),
        );

        (Arc::new(Mutex::new(app)), link_map)
    }

    #[test]
    fn apply_links_marks_linked_gh_workflow_succeeded() {
        let (app, mut link_map) = make_app_with_link_scenario();
        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Succeeded);
        assert_eq!(a.workflow_groups[0].jobs[0].status, BuildStatus::Succeeded);
    }

    #[test]
    fn apply_links_no_effect_when_cp_not_running() {
        let mut app = App::new();
        let mut job = Bar::new("build".into());
        job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded, // Not Running
            pending_link: false,
        });

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());

        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        // GH workflow should still be Running
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Running);
    }

    #[test]
    fn apply_links_unlinked_gh_workflow_unchanged() {
        let mut app = App::new();
        // Two GH workflows: CI (linked) and Lint (unlinked)
        let mut ci_job = Bar::new("build".into());
        ci_job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![ci_job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        let mut lint_job = Bar::new("lint".into());
        lint_job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "Lint".into(),
            jobs: vec![lint_job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(200),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        });

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());

        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        // CI should be Succeeded (linked)
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Succeeded);
        // Lint should remain Running (unlinked)
        assert_eq!(a.workflow_groups[1].summary_status, BuildStatus::Running);
    }

    #[test]
    fn apply_links_multiple_pipelines_multiple_workflows() {
        let mut app = App::new();
        for (i, name) in ["Frontend CI", "Backend CI"].iter().enumerate() {
            let mut job = Bar::new("build".into());
            job.set_status(BuildStatus::Running);
            app.workflow_groups.push(WorkflowGroup {
                name: name.to_string(),
                jobs: vec![job],
                gone: false,
                summary_status: BuildStatus::Running,
                run_id: Some(100 + i as u64),
                category: WorkflowCategory::default(),
                linked_pipeline: None,
            });
        }
        for name in &["frontend-pipe", "backend-pipe"] {
            app.pipeline_groups.push(PipelineGroup {
                name: name.to_string(),
                stages: vec![],
                gone: false,
                summary_status: BuildStatus::Running,
                pending_link: false,
            });
        }

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "frontend-pipe".into(),
            "Frontend CI".into(),
            "b".into(),
            "fe/".into(),
        );
        link_map.add_discovered(
            "backend-pipe".into(),
            "Backend CI".into(),
            "b".into(),
            "be/".into(),
        );

        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Succeeded);
        assert_eq!(a.workflow_groups[1].summary_status, BuildStatus::Succeeded);
    }

    #[test]
    fn apply_links_already_succeeded_no_double_mark() {
        let mut app = App::new();
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded, // Already done
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        });

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());

        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        // Should remain Succeeded (no-op)
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Succeeded);
    }

    #[test]
    fn apply_links_does_not_suppress_new_run_id() {
        // CP running, workflow was stopped (run 100), now a new run (200) appears
        let mut app = App::new();
        let mut job = Bar::new("build".into());
        job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(200), // new run
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        });

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());

        // Simulate that run 100 was already stopped
        let mut stopped_runs = HashMap::new();
        stopped_runs.insert("CI".to_string(), 100);

        apply_links(&app, &mut link_map, &mut stopped_runs);

        let a = app.lock().unwrap();
        // New run (200) should NOT be suppressed
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Running);
    }

    #[test]
    fn apply_links_suppresses_same_run_id_only_once() {
        let (app, mut link_map) = make_app_with_link_scenario();
        let mut stopped_runs = HashMap::new();

        // First call: suppresses run 100
        apply_links(&app, &mut link_map, &mut stopped_runs);
        assert_eq!(stopped_runs.get("CI"), Some(&100));
        {
            let a = app.lock().unwrap();
            assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Succeeded);
        }

        // Simulate next poll: GH API still reports run 100 as Running
        {
            let mut a = app.lock().unwrap();
            a.workflow_groups[0].summary_status = BuildStatus::Running;
        }

        // Second call with same run_id: should be no-op (already stopped)
        apply_links(&app, &mut link_map, &mut stopped_runs);
        let a = app.lock().unwrap();
        // stays Running because apply_links skips already-stopped run_id
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Running);
    }

    #[test]
    fn apply_links_clears_stopped_when_cp_finishes() {
        let (app, mut link_map) = make_app_with_link_scenario();
        let mut stopped_runs = HashMap::new();

        // First call: suppresses run 100
        apply_links(&app, &mut link_map, &mut stopped_runs);
        assert!(stopped_runs.contains_key("CI"));

        // CP finishes
        {
            let mut a = app.lock().unwrap();
            a.pipeline_groups[0].summary_status = BuildStatus::Succeeded;
        }

        apply_links(&app, &mut link_map, &mut stopped_runs);

        // stopped_runs should be cleared for "CI"
        assert!(!stopped_runs.contains_key("CI"));
    }

    #[test]
    fn apply_links_workflow_with_no_run_id() {
        let mut app = App::new();
        let mut job = Bar::new("build".into());
        job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        });
        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());
        let mut stopped = HashMap::new();
        apply_links(&app, &mut link_map, &mut stopped);
        let a = app.lock().unwrap();
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Succeeded);
        assert!(stopped.is_empty()); // No run_id, so nothing tracked
    }

    #[test]
    fn apply_links_only_running_jobs_marked_succeeded() {
        let mut app = App::new();
        let mut running_job = Bar::new("build".into());
        running_job.set_status(BuildStatus::Running);
        let mut failed_job = Bar::new("test".into());
        failed_job.set_status(BuildStatus::Failed);
        let mut succeeded_job = Bar::new("lint".into());
        succeeded_job.set_status(BuildStatus::Succeeded);
        let idle_job = Bar::new("deploy".into());
        // Idle is default from Bar::new, no set_status needed
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![running_job, failed_job, succeeded_job, idle_job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        });
        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());
        apply_links(&app, &mut link_map, &mut HashMap::new());
        let a = app.lock().unwrap();
        assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Succeeded);
        assert_eq!(a.workflow_groups[0].jobs[0].status, BuildStatus::Succeeded); // was Running
        assert_eq!(a.workflow_groups[0].jobs[1].status, BuildStatus::Failed); // stays Failed
        assert_eq!(a.workflow_groups[0].jobs[2].status, BuildStatus::Succeeded); // stays Succeeded
        assert_eq!(a.workflow_groups[0].jobs[3].status, BuildStatus::Idle); // stays Idle
    }

    #[test]
    fn apply_links_records_workflow_completion() {
        let mut app = App::new();
        // GH workflow already Succeeded
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        let mut stopped = HashMap::new();

        // apply_links should record the completion
        apply_links(&app, &mut link_map, &mut stopped);

        // Verify by trying to correlate a new pipeline — should find "CI"
        let result = link_map.try_correlate("new-pipe");
        assert_eq!(result, Some("CI".to_string()));
    }

    #[test]
    fn try_correlate_multiple_completions_uses_last() {
        let mut map = LinkMap::new();
        map.record_workflow_completion("CI");
        map.record_workflow_completion("Deploy");
        let result = map.try_correlate("new-pipe");
        assert_eq!(result, Some("Deploy".to_string()));
    }

    #[test]
    fn s3_keys_slash_only() {
        assert!(!s3_keys_match("/", "my-app"));
    }

    // --- cache serialization tests ---

    #[test]
    fn to_cache_empty_link_map() {
        let map = LinkMap::new();
        let cache = map.to_cache("2026-03-27T00:00:00Z");
        assert_eq!(cache.schema_version, 1);
        assert_eq!(cache.discovered_at, "2026-03-27T00:00:00Z");
        assert!(cache.links.is_empty());
    }

    #[test]
    fn to_cache_preserves_yaml_link() {
        let mut map = LinkMap::new();
        map.add_discovered(
            "my-pipe".into(),
            "CI".into(),
            "bucket".into(),
            "key.zip".into(),
        );
        let cache = map.to_cache("2026-03-27T00:00:00Z");
        assert_eq!(cache.links.len(), 1);
        assert_eq!(cache.links[0].pipeline_name, "my-pipe");
        assert_eq!(cache.links[0].workflow_name, "CI");
        assert_eq!(cache.links[0].s3_bucket, "bucket");
        assert_eq!(cache.links[0].s3_key, "key.zip");
        assert!(matches!(
            cache.links[0].source,
            CachedLinkSource::YamlDiscovered
        ));
    }

    #[test]
    fn to_cache_preserves_runtime_link() {
        let mut map = LinkMap::new();
        map.record_workflow_completion("CI");
        map.try_correlate("deploy-pipe");
        let cache = map.to_cache("2026-03-27T00:00:00Z");
        assert_eq!(cache.links.len(), 1);
        assert!(matches!(
            cache.links[0].source,
            CachedLinkSource::RuntimeCorrelated
        ));
    }

    #[test]
    fn load_from_cache_round_trip() {
        let mut map = LinkMap::new();
        map.add_discovered("pipe-a".into(), "WF-A".into(), "b".into(), "k".into());
        let cache = map.to_cache("2026-03-27T00:00:00Z");

        let mut map2 = LinkMap::new();
        map2.load_from_cache(cache);
        assert_eq!(map2.workflow_for_pipeline("pipe-a"), Some("WF-A"));
        assert_eq!(map2.links().len(), 1);
    }

    #[test]
    fn load_from_cache_replaces_existing() {
        let mut map = LinkMap::new();
        map.add_discovered("old".into(), "OLD-WF".into(), "b".into(), "k".into());

        let mut fresh = LinkMap::new();
        fresh.add_discovered("new".into(), "NEW-WF".into(), "b2".into(), "k2".into());
        let cache = fresh.to_cache("2026-03-27T00:00:00Z");

        map.load_from_cache(cache);
        assert_eq!(map.workflow_for_pipeline("old"), None);
        assert_eq!(map.workflow_for_pipeline("new"), Some("NEW-WF"));
    }

    #[test]
    fn clear_removes_links_and_completions() {
        let mut map = LinkMap::new();
        map.add_discovered("p".into(), "w".into(), "b".into(), "k".into());
        map.record_workflow_completion("w");
        map.clear();
        assert!(map.links().is_empty());
        assert_eq!(map.try_correlate("p"), None);
    }

    // --- pending_link tests ---

    #[test]
    fn pending_link_set_when_gh_running_cp_idle() {
        let mut app = App::new();
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        });

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());

        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        assert!(
            a.pipeline_groups[0].pending_link,
            "linked CP should be pending when GH is Running and CP is Idle"
        );
    }

    #[test]
    fn pending_link_cleared_when_cp_running() {
        let mut app = App::new();
        let mut job = Bar::new("build".into());
        job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        });

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());

        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        assert!(
            !a.pipeline_groups[0].pending_link,
            "pending_link should be false when CP is Running"
        );
    }

    #[test]
    fn pending_link_false_for_unlinked() {
        let mut app = App::new();
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "unlinked-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        });

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new(); // no links
        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        assert!(
            !a.pipeline_groups[0].pending_link,
            "unlinked pipeline should not have pending_link"
        );
    }

    #[test]
    fn pending_link_false_when_gh_succeeded() {
        let mut app = App::new();
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Succeeded,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        });

        let app = Arc::new(Mutex::new(app));
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy-pipe".into(), "CI".into(), "b".into(), "k".into());

        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        assert!(
            !a.pipeline_groups[0].pending_link,
            "pending_link should be false when GH workflow Succeeded"
        );
    }
}
