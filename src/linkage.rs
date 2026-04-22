use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::model::BuildStatus;
use crate::poller::{ActionsClient, PipelineClient};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CachedLink {
    pub pipeline_name: String,
    pub workflow_name: String,
    pub s3_bucket: String,
    pub s3_key: String,
    pub source: CachedLinkSource,
    /// Job IDs that directly upload to this pipeline's S3 source.
    #[serde(default)]
    pub job_ids: Vec<String>,
    /// Job IDs that are upstream dependencies of `job_ids` (via `needs:` chain).
    #[serde(default)]
    pub dep_job_ids: Vec<String>,
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
    #[serde(default)]
    pub job_assignment: Option<JobAssignment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkSource {
    YamlDiscovered,
    RuntimeCorrelated,
}

#[derive(Debug, Clone)]
pub struct PipelineLink {
    pub pipeline_name: String,
    pub workflow_name: String,
    pub s3_bucket: String,
    pub s3_key: String,
    pub source: LinkSource,
    /// Job IDs that directly upload to this pipeline's S3 source.
    pub job_ids: Vec<String>,
    /// Upstream dependency job IDs (via `needs:` chain).
    pub dep_job_ids: Vec<String>,
}

pub struct LinkMap {
    links: Vec<PipelineLink>,
    /// Track recent GH workflow completions for runtime correlation.
    recent_completions: Vec<(String, Instant)>,
    /// Run IDs already recorded, to avoid re-recording the same completion.
    recorded_run_ids: HashSet<u64>,
}

/// Correlation window: a CP pipeline must start within this duration
/// of a GH workflow completion to be considered linked.
const CORRELATION_WINDOW_SECS: u64 = 30;

impl LinkMap {
    pub fn new() -> Self {
        Self {
            links: Vec::new(),
            recent_completions: Vec::new(),
            recorded_run_ids: HashSet::new(),
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
            job_ids: Vec::new(),
            dep_job_ids: Vec::new(),
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
        self.prune_expired();
        let cutoff = std::time::Duration::from_secs(CORRELATION_WINDOW_SECS);
        let already_recorded = self
            .recent_completions
            .iter()
            .any(|(n, t)| n == workflow_name && t.elapsed() < cutoff);
        if !already_recorded {
            self.recent_completions
                .push((workflow_name.to_string(), Instant::now()));
        }
        // Cap to prevent unbounded growth in long-running sessions
        if self.recent_completions.len() > 500 {
            self.recent_completions
                .drain(..self.recent_completions.len() - 500);
        }
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
                job_ids: Vec::new(),
                dep_job_ids: Vec::new(),
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
                job_ids: cl.job_ids,
                dep_job_ids: cl.dep_job_ids,
            });
        }
    }

    /// Serialize current links to a cache struct.
    pub fn to_cache(
        &self,
        discovered_at: &str,
        job_assignment: Option<JobAssignment>,
    ) -> LinkCache {
        LinkCache {
            schema_version: 2,
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
                    job_ids: l.job_ids.clone(),
                    dep_job_ids: l.dep_job_ids.clone(),
                })
                .collect(),
            job_assignment,
        }
    }

    /// Remove all links and recent completions (before re-discovery).
    pub fn clear(&mut self) {
        self.links.clear();
        self.recent_completions.clear();
    }

    /// Check whether a workflow completed recently (within correlation window).
    pub fn has_recent_completion(&self, workflow_name: &str) -> bool {
        let cutoff = std::time::Duration::from_secs(CORRELATION_WINDOW_SECS);
        self.recent_completions
            .iter()
            .any(|(n, t)| n == workflow_name && t.elapsed() < cutoff)
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
            if cache.schema_version != 1 && cache.schema_version != 2 {
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

/// Discover GH workflow <-> CP pipeline links by matching S3 source configs.
/// Returns the link map and the workflow files used for discovery.
pub async fn discover_links(
    aws: &dyn PipelineClient,
    gh: &dyn ActionsClient,
) -> (LinkMap, Vec<crate::poller::WorkflowFile>) {
    let mut link_map = LinkMap::new();

    let pipeline_names = match aws.list_pipeline_names().await {
        Ok(names) => names,
        Err(e) => {
            tracing::warn!(error = %e, "failed to list pipelines for link discovery");
            return (link_map, Vec::new());
        }
    };

    // Fetch pipeline definitions in parallel
    let def_futs: Vec<_> = pipeline_names
        .iter()
        .map(|n| aws.get_pipeline_definition(n))
        .collect();
    let definitions: Vec<_> = futures::future::join_all(def_futs)
        .await
        .into_iter()
        .filter_map(|r| match r {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!(error = %e, "failed to get pipeline definition");
                None
            }
        })
        .collect();

    let workflow_files = match gh.fetch_workflow_files().await {
        Ok(wf) => wf,
        Err(e) => {
            tracing::warn!(error = %e, "failed to fetch workflow files for link discovery");
            return (link_map, Vec::new());
        }
    };

    // Log discovery inputs for diagnostics
    let s3_pipeline_count = definitions.iter().filter(|d| d.source_s3.is_some()).count();
    tracing::info!(
        pipelines = pipeline_names.len(),
        with_s3_source = s3_pipeline_count,
        workflow_files = workflow_files.len(),
        total_s3_uploads = workflow_files
            .iter()
            .map(|wf| wf.s3_uploads.len())
            .sum::<usize>(),
        "link discovery inputs"
    );
    for def in &definitions {
        if let Some(s3) = &def.source_s3 {
            tracing::debug!(
                pipeline = %def.name,
                bucket = %s3.bucket,
                key = %s3.object_key,
                "pipeline S3 source"
            );
        }
    }
    for wf in &workflow_files {
        for upload in &wf.s3_uploads {
            tracing::debug!(
                workflow = %wf.name,
                bucket = %upload.bucket,
                key = %upload.key,
                "workflow S3 upload"
            );
        }
    }

    // Match: CP source S3 key matches GH upload S3 key
    for def in &definitions {
        if let Some(s3) = &def.source_s3 {
            for wf in &workflow_files {
                for upload in &wf.s3_uploads {
                    if s3_keys_match(&s3.object_key, &upload.key) {
                        link_map.add_discovered(
                            def.name.clone(),
                            wf.name.clone(),
                            s3.bucket.clone(),
                            s3.object_key.clone(),
                        );
                    }
                }
            }
        }
    }

    tracing::info!(links = link_map.links().len(), "link discovery complete");
    (link_map, workflow_files)
}

/// Update each WorkflowGroup's linked_pipeline from the LinkMap.
pub fn sync_linked_pipelines(app: &Arc<Mutex<App>>, link_map: &LinkMap) {
    let mut a = app.lock().expect("app mutex poisoned");
    for wg in &mut a.workflow_groups {
        wg.linked_pipeline = link_map
            .links()
            .iter()
            .find(|l| l.workflow_name == wg.name)
            .map(|l| l.pipeline_name.clone());
    }
}

/// Run link discovery, persist cache, and update app state.
pub async fn run_discovery(
    aws: &dyn PipelineClient,
    gh: &dyn ActionsClient,
    app: &Arc<Mutex<App>>,
    cache_path: &std::path::Path,
) -> LinkMap {
    app.lock().expect("app mutex poisoned").linkage_discovering = true;
    let (link_map, workflow_files) = discover_links(aws, gh).await;

    if !link_map.links().is_empty() {
        // Compute per-job pipeline assignments for pipeline-centric UI
        let assignment = assign_jobs_to_pipelines(&workflow_files, &link_map);
        let has_jobs = assignment
            .pipeline_jobs
            .values()
            .any(|(_, jobs)| !jobs.is_empty())
            || assignment
                .shared_jobs
                .iter()
                .any(|(_, jobs)| !jobs.is_empty());

        let cached_assignment = if has_jobs {
            Some(assignment.clone())
        } else {
            None
        };

        let cache = link_map.to_cache(&Utc::now().to_rfc3339(), cached_assignment);
        if let Err(e) = save_link_cache(cache_path, &cache) {
            tracing::warn!(error = %e, "failed to save link cache");
        }

        if has_jobs {
            tracing::info!(
                pipelines = assignment.pipeline_jobs.len(),
                shared_workflows = assignment.shared_jobs.len(),
                "computed job assignments for pipeline-centric UI"
            );
            app.lock().expect("app mutex poisoned").job_assignment = Some(assignment);
        }
    }

    sync_linked_pipelines(app, &link_map);
    app.lock().expect("app mutex poisoned").linkage_discovering = false;
    link_map
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

    // Record GH workflow completions (non-Running terminal states).
    // Only record once per run_id to prevent the correlation window
    // from being refreshed every poll cycle.
    for wg in &a.workflow_groups {
        if (wg.summary_status == BuildStatus::Succeeded || wg.summary_status == BuildStatus::Failed)
            && wg
                .run_id
                .is_some_and(|id| !link_map.recorded_run_ids.contains(&id))
        {
            if let Some(id) = wg.run_id {
                link_map.recorded_run_ids.insert(id);
            }
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
            let recently_completed = link_map.has_recent_completion(wf_name);
            pg.pending_link = (linked_wf_running || recently_completed)
                && pg.summary_status != BuildStatus::Running;
        } else {
            pg.pending_link = false;
        }
    }
}

/// Result of assigning workflow jobs to pipelines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobAssignment {
    /// Jobs assigned to exactly one pipeline, keyed by pipeline name.
    /// Value is (workflow_name, ordered list of job display names).
    pub pipeline_jobs: HashMap<String, (String, Vec<String>)>,
    /// Jobs shared across multiple pipelines (or orphans).
    /// (workflow_name, list of job display names).
    pub shared_jobs: Vec<(String, Vec<String>)>,
}

/// Assign workflow jobs to pipelines using per-job S3 uploads and `needs:` chains.
///
/// Algorithm:
/// 1. For each pipeline link, find jobs whose S3 uploads match the pipeline's S3 key
/// 2. Walk the `needs:` graph backward to collect upstream dependencies
/// 3. Jobs assigned to >1 pipeline → shared
/// 4. Orphan jobs (not reachable from any pipeline) → shared
/// 5. Jobs within each pipeline are topologically sorted (deps first, upload job last)
pub fn assign_jobs_to_pipelines(
    workflow_files: &[crate::poller::WorkflowFile],
    link_map: &LinkMap,
) -> JobAssignment {
    use std::collections::{HashMap, HashSet};

    // Build a map of (workflow_name) → Vec<JobS3Info> for quick lookup
    let mut workflow_jobs_map: HashMap<&str, &[crate::poller::JobS3Info]> = HashMap::new();
    for wf in workflow_files {
        if !wf.jobs.is_empty() {
            workflow_jobs_map.insert(&wf.name, &wf.jobs);
        }
    }

    // For each pipeline, find which jobs belong to it
    // Key: (workflow_name, job_id), Value: set of pipeline names
    let mut job_to_pipelines: HashMap<(&str, &str), HashSet<&str>> = HashMap::new();
    // Track which jobs belong to which pipeline (before shared detection)
    let mut pipeline_job_ids: HashMap<&str, Vec<(&str, &str)>> = HashMap::new(); // pipeline -> [(wf_name, job_id)]

    for link in link_map.links() {
        let wf_name = link.workflow_name.as_str();
        let jobs = match workflow_jobs_map.get(wf_name) {
            Some(j) => *j,
            None => continue,
        };

        // Step 1: Find jobs with matching S3 uploads
        let mut matched_job_ids: HashSet<&str> = HashSet::new();
        for job in jobs {
            for upload in &job.s3_uploads {
                if s3_keys_match(&link.s3_key, &upload.key) {
                    matched_job_ids.insert(&job.job_id);
                }
            }
        }

        // Step 2: Walk needs: chain backward from matched jobs
        let mut all_job_ids: HashSet<&str> = matched_job_ids.clone();
        let mut frontier: Vec<&str> = matched_job_ids.iter().copied().collect();
        while let Some(job_id) = frontier.pop() {
            if let Some(job) = jobs.iter().find(|j| j.job_id == job_id) {
                for dep in &job.needs {
                    if all_job_ids.insert(dep.as_str()) {
                        frontier.push(dep.as_str());
                    }
                }
            }
        }

        let pipeline_name = link.pipeline_name.as_str();
        for job_id in &all_job_ids {
            job_to_pipelines
                .entry((wf_name, job_id))
                .or_default()
                .insert(pipeline_name);
        }
        pipeline_job_ids
            .entry(pipeline_name)
            .or_default()
            .extend(all_job_ids.iter().map(|jid| (wf_name, *jid)));
    }

    // Step 3: Split into shared vs pipeline-unique
    let shared_job_keys: HashSet<(&str, &str)> = job_to_pipelines
        .iter()
        .filter(|(_, pipelines)| pipelines.len() > 1)
        .map(|(key, _)| *key)
        .collect();

    // Step 4: Detect orphans (jobs in linked workflows but not assigned to any pipeline)
    let mut all_assigned: HashSet<(&str, &str)> = HashSet::new();
    for key in job_to_pipelines.keys() {
        all_assigned.insert(*key);
    }
    let mut orphan_keys: Vec<(&str, &str)> = Vec::new();
    for link in link_map.links() {
        if let Some(jobs) = workflow_jobs_map.get(link.workflow_name.as_str()) {
            for job in *jobs {
                let key = (link.workflow_name.as_str(), job.job_id.as_str());
                if !all_assigned.contains(&key) {
                    orphan_keys.push(key);
                }
            }
        }
    }

    // Build shared jobs list: shared + orphans, grouped by workflow
    let mut shared_by_wf: HashMap<&str, Vec<&str>> = HashMap::new();
    for (wf_name, job_id) in shared_job_keys.iter().chain(orphan_keys.iter()) {
        shared_by_wf.entry(wf_name).or_default().push(job_id);
    }

    // Topological sort helper for job ordering within a group
    fn topo_sort_jobs<'a>(
        job_ids: &[&'a str],
        jobs: &'a [crate::poller::JobS3Info],
    ) -> Vec<&'a str> {
        // Build adjacency: job_id → jobs it depends on (that are in our set)
        let id_set: HashSet<&str> = job_ids.iter().copied().collect();
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

        for jid in &id_set {
            in_degree.entry(jid).or_insert(0);
        }
        for job in jobs {
            if !id_set.contains(job.job_id.as_str()) {
                continue;
            }
            for dep in &job.needs {
                if id_set.contains(dep.as_str()) {
                    *in_degree.entry(job.job_id.as_str()).or_insert(0) += 1;
                    dependents
                        .entry(dep.as_str())
                        .or_default()
                        .push(job.job_id.as_str());
                }
            }
        }

        let mut queue: Vec<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();
        queue.sort(); // Stable alphabetical for determinism
        let mut result = Vec::new();
        while let Some(jid) = queue.pop() {
            result.push(jid);
            if let Some(deps) = dependents.get(jid) {
                for &dep in deps {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(dep);
                            queue.sort();
                        }
                    }
                }
            }
        }
        result
    }

    // Build pipeline_jobs: only unique (non-shared) jobs, topo-sorted
    let mut pipeline_jobs: HashMap<String, (String, Vec<String>)> = HashMap::new();
    for link in link_map.links() {
        let pipeline_name = &link.pipeline_name;
        let wf_name = link.workflow_name.as_str();
        if let Some(jobs) = workflow_jobs_map.get(wf_name) {
            let unique_ids: Vec<&str> = pipeline_job_ids
                .get(pipeline_name.as_str())
                .map(|ids| {
                    ids.iter()
                        .filter(|(_, jid)| !shared_job_keys.contains(&(wf_name, *jid)))
                        .map(|(_, jid)| *jid)
                        .collect()
                })
                .unwrap_or_default();

            let sorted = topo_sort_jobs(&unique_ids, jobs);
            let names: Vec<String> = sorted
                .iter()
                .filter_map(|jid| jobs.iter().find(|j| j.job_id == *jid))
                .map(|j| j.job_name.clone())
                .collect();

            pipeline_jobs.insert(pipeline_name.clone(), (link.workflow_name.clone(), names));
        }
    }

    // Build shared_jobs, topo-sorted per workflow
    let mut shared_jobs: Vec<(String, Vec<String>)> = Vec::new();
    for (wf_name, job_ids) in &shared_by_wf {
        if let Some(jobs) = workflow_jobs_map.get(*wf_name) {
            let sorted = topo_sort_jobs(job_ids, jobs);
            let names: Vec<String> = sorted
                .iter()
                .filter_map(|jid| jobs.iter().find(|j| j.job_id == *jid))
                .map(|j| j.job_name.clone())
                .collect();
            shared_jobs.push((wf_name.to_string(), names));
        }
    }

    JobAssignment {
        pipeline_jobs,
        shared_jobs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bar, PipelineGroup, WorkflowCategory, WorkflowGroup};
    use crate::poller::{
        ActionsClient, JobInfo, PipelineClient, PipelineDefinition, PipelineState, S3Source,
        S3Upload, WorkflowFile,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    struct LinkMockPipeline {
        names: Vec<String>,
        definitions: Vec<PipelineDefinition>,
    }

    #[async_trait::async_trait]
    impl PipelineClient for LinkMockPipeline {
        async fn list_pipeline_names(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.names.clone())
        }
        async fn get_pipeline_state(&self, _name: &str) -> anyhow::Result<PipelineState> {
            anyhow::bail!("not used in discovery tests")
        }
        async fn get_pipeline_definition(&self, name: &str) -> anyhow::Result<PipelineDefinition> {
            Ok(self
                .definitions
                .iter()
                .find(|d| d.name == name)
                .cloned()
                .unwrap_or(PipelineDefinition {
                    name: name.into(),
                    source_s3: None,
                }))
        }
    }

    /// Mock that takes raw YAML strings and runs them through the real
    /// `parse_workflow_yaml` pipeline — exercises extract_s3_paths and all parsing.
    struct LinkMockActions {
        /// (filename, raw YAML content) pairs — parsed via `parse_workflow_yaml`
        workflow_yamls: Vec<(&'static str, &'static str)>,
    }

    #[async_trait::async_trait]
    impl ActionsClient for LinkMockActions {
        async fn list_latest_runs(&self) -> anyhow::Result<crate::poller::RunsPage> {
            Ok(crate::poller::RunsPage::default())
        }
        async fn fetch_run_jobs(&self, _: u64) -> anyhow::Result<Vec<JobInfo>> {
            Ok(vec![])
        }
        async fn fetch_workflow_files(&self) -> anyhow::Result<Vec<WorkflowFile>> {
            use crate::poller::github::parse_workflow_yaml;
            Ok(self
                .workflow_yamls
                .iter()
                .filter_map(|(filename, content)| parse_workflow_yaml(filename, content))
                .filter(|wf| !wf.s3_uploads.is_empty())
                .collect())
        }
    }

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
        let cache = map.to_cache("2026-03-27T12:00:00Z", None);

        save_link_cache(&path, &cache).unwrap();

        let loaded = load_link_cache(&path).unwrap().unwrap();
        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.discovered_at, "2026-03-27T12:00:00Z");
        assert_eq!(loaded.links.len(), 1);
        assert_eq!(loaded.links[0].pipeline_name, "deploy");
    }

    #[test]
    fn save_and_load_job_assignment_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cibars-links.toml");

        let mut map = LinkMap::new();
        map.add_discovered(
            "pipe".into(),
            "CI".into(),
            "bucket".into(),
            "key.zip".into(),
        );

        let assignment = JobAssignment {
            pipeline_jobs: {
                let mut m = HashMap::new();
                m.insert(
                    "pipe".to_string(),
                    ("CI".to_string(), vec!["Build".to_string()]),
                );
                m
            },
            shared_jobs: vec![("CI".to_string(), vec!["Test".to_string()])],
        };

        let cache = map.to_cache("2026-03-28T12:00:00Z", Some(assignment));
        save_link_cache(&path, &cache).unwrap();

        let loaded = load_link_cache(&path).unwrap().unwrap();
        let ja = loaded
            .job_assignment
            .expect("job_assignment should roundtrip");
        assert_eq!(ja.pipeline_jobs["pipe"].1, vec!["Build"]);
        assert_eq!(ja.shared_jobs[0].1, vec!["Test"]);
    }

    #[test]
    fn load_v1_cache_with_serde_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cibars-links.toml");
        let v1_content = r#"
schema_version = 1
discovered_at = "2026-03-27T12:00:00Z"

[[links]]
pipeline_name = "deploy"
workflow_name = "CI"
s3_bucket = "bucket"
s3_key = "art.zip"
source = "YamlDiscovered"
"#;
        std::fs::write(&path, v1_content).unwrap();
        let loaded = load_link_cache(&path).unwrap().unwrap();
        assert_eq!(loaded.schema_version, 1);
        assert_eq!(loaded.links[0].job_ids, Vec::<String>::new());
        assert_eq!(loaded.links[0].dep_job_ids, Vec::<String>::new());
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
            job_assignment: None,
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
    fn has_recent_completion_within_window() {
        let mut map = LinkMap::new();
        map.record_workflow_completion("CI");
        assert!(map.has_recent_completion("CI"));
        assert!(!map.has_recent_completion("Other"));
    }

    #[test]
    fn has_recent_completion_expired() {
        let mut map = LinkMap::new();
        map.recent_completions
            .push(("CI".to_string(), Instant::now() - Duration::from_secs(60)));
        assert!(!map.has_recent_completion("CI"));
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
        let cache = map.to_cache("2026-03-27T00:00:00Z", None);
        assert_eq!(cache.schema_version, 2);
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
        let cache = map.to_cache("2026-03-27T00:00:00Z", None);
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
        let cache = map.to_cache("2026-03-27T00:00:00Z", None);
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
        let cache = map.to_cache("2026-03-27T00:00:00Z", None);

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
        let cache = fresh.to_cache("2026-03-27T00:00:00Z", None);

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
    fn pending_link_true_when_gh_recently_completed() {
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

        // apply_links records "CI" into recent_completions (since Succeeded)
        apply_links(&app, &mut link_map, &mut HashMap::new());

        let a = app.lock().unwrap();
        assert!(
            a.pipeline_groups[0].pending_link,
            "pending_link should be true when GH recently completed and CP still Idle"
        );
    }

    // --- e2e discovery tests (raw YAML → parse → discover → cache → apply) ---

    #[tokio::test]
    async fn discover_links_matches_s3_keys() {
        let aws = LinkMockPipeline {
            names: vec!["deploy-pipe".into()],
            definitions: vec![PipelineDefinition {
                name: "deploy-pipe".into(),
                source_s3: Some(S3Source {
                    bucket: "my-bucket".into(),
                    object_key: "artifacts/app.zip".into(),
                }),
            }],
        };
        let gh = LinkMockActions {
            workflow_yamls: vec![(
                "ci.yml",
                "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: aws s3 cp app.zip s3://my-bucket/artifacts/app.zip\n",
            )],
        };

        let (link_map, _wf) = super::discover_links(&aws, &gh).await;
        assert_eq!(link_map.links().len(), 1);
        assert_eq!(link_map.workflow_for_pipeline("deploy-pipe"), Some("CI"));
    }

    #[tokio::test]
    async fn discover_links_no_match_different_keys() {
        let aws = LinkMockPipeline {
            names: vec!["pipe".into()],
            definitions: vec![PipelineDefinition {
                name: "pipe".into(),
                source_s3: Some(S3Source {
                    bucket: "b".into(),
                    object_key: "path/a.zip".into(),
                }),
            }],
        };
        let gh = LinkMockActions {
            workflow_yamls: vec![(
                "ci.yml",
                "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: aws s3 cp x.zip s3://b/path/b.zip\n",
            )],
        };

        let (link_map, _wf) = super::discover_links(&aws, &gh).await;
        assert!(link_map.links().is_empty());
    }

    #[tokio::test]
    async fn discover_links_multiple_pipelines_multiple_workflows() {
        let aws = LinkMockPipeline {
            names: vec!["frontend-pipe".into(), "backend-pipe".into()],
            definitions: vec![
                PipelineDefinition {
                    name: "frontend-pipe".into(),
                    source_s3: Some(S3Source {
                        bucket: "b".into(),
                        object_key: "fe/build.zip".into(),
                    }),
                },
                PipelineDefinition {
                    name: "backend-pipe".into(),
                    source_s3: Some(S3Source {
                        bucket: "b".into(),
                        object_key: "be/build.zip".into(),
                    }),
                },
            ],
        };
        let gh = LinkMockActions {
            workflow_yamls: vec![
                (
                    "frontend.yml",
                    "name: Frontend CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: aws s3 cp fe.zip s3://b/fe/build.zip\n",
                ),
                (
                    "backend.yml",
                    "name: Backend CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: aws s3 cp be.zip s3://b/be/build.zip\n",
                ),
            ],
        };

        let (link_map, _wf) = super::discover_links(&aws, &gh).await;
        assert_eq!(link_map.links().len(), 2);
        assert_eq!(
            link_map.workflow_for_pipeline("frontend-pipe"),
            Some("Frontend CI")
        );
        assert_eq!(
            link_map.workflow_for_pipeline("backend-pipe"),
            Some("Backend CI")
        );
    }

    #[tokio::test]
    async fn discover_links_with_github_actions_expressions() {
        // Real-world scenario: bucket name contains ${{ vars.AWS_ACCOUNT_ID }}
        let aws = LinkMockPipeline {
            names: vec!["backend-pipe".into(), "frontend-pipe".into()],
            definitions: vec![
                PipelineDefinition {
                    name: "backend-pipe".into(),
                    source_s3: Some(S3Source {
                        bucket: "attracr-ci-artifacts-123456".into(),
                        object_key: "backend/backend-source.zip".into(),
                    }),
                },
                PipelineDefinition {
                    name: "frontend-pipe".into(),
                    source_s3: Some(S3Source {
                        bucket: "attracr-ci-artifacts-123456".into(),
                        object_key: "frontend/frontend-artifacts.zip".into(),
                    }),
                },
            ],
        };
        let gh = LinkMockActions {
            workflow_yamls: vec![(
                "ci.yml",
                r#"name: CI
on: push
jobs:
  build-backend:
    runs-on: ubuntu-latest
    steps:
      - run: |
          zip -r backend-source.zip infrastructure/ backend/
          aws s3 cp backend-source.zip s3://attracr-ci-artifacts-${{ vars.AWS_ACCOUNT_ID }}/backend/backend-source.zip
  build-frontend:
    runs-on: ubuntu-latest
    steps:
      - run: |
          aws s3 cp frontend-artifacts.zip s3://attracr-ci-artifacts-${{ vars.AWS_ACCOUNT_ID }}/frontend/frontend-artifacts.zip
"#,
            )],
        };

        let (link_map, _wf) = super::discover_links(&aws, &gh).await;
        assert_eq!(
            link_map.links().len(),
            2,
            "should match despite ${{{{ }}}} in bucket name"
        );
        assert_eq!(link_map.workflow_for_pipeline("backend-pipe"), Some("CI"));
        assert_eq!(link_map.workflow_for_pipeline("frontend-pipe"), Some("CI"));
    }

    #[tokio::test]
    async fn run_discovery_saves_cache_file() {
        let aws = LinkMockPipeline {
            names: vec!["deploy".into()],
            definitions: vec![PipelineDefinition {
                name: "deploy".into(),
                source_s3: Some(S3Source {
                    bucket: "b".into(),
                    object_key: "art.zip".into(),
                }),
            }],
        };
        let gh = LinkMockActions {
            workflow_yamls: vec![(
                "deploy.yml",
                "name: CI\non: push\njobs:\n  d:\n    runs-on: ubuntu-latest\n    steps:\n      - run: aws s3 cp x s3://b/art.zip\n",
            )],
        };

        let app = Arc::new(Mutex::new(App::new()));
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join(".cibars-links.toml");

        let link_map = super::run_discovery(&aws, &gh, &app, &cache_path).await;
        assert_eq!(link_map.links().len(), 1);

        // Cache file should exist
        assert!(cache_path.exists());

        // Verify cache content
        let loaded = super::load_link_cache(&cache_path).unwrap().unwrap();
        assert_eq!(loaded.links.len(), 1);
        assert_eq!(loaded.links[0].pipeline_name, "deploy");
        assert_eq!(loaded.links[0].workflow_name, "CI");

        // linkage_discovering should be false after completion
        let a = app.lock().unwrap();
        assert!(!a.linkage_discovering);
    }

    #[tokio::test]
    async fn run_discovery_skips_cache_when_no_links() {
        let aws = LinkMockPipeline {
            names: vec!["pipe".into()],
            definitions: vec![PipelineDefinition {
                name: "pipe".into(),
                source_s3: None,
            }],
        };
        let gh = LinkMockActions {
            workflow_yamls: vec![],
        };

        let app = Arc::new(Mutex::new(App::new()));
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join(".cibars-links.toml");

        let link_map = super::run_discovery(&aws, &gh, &app, &cache_path).await;
        assert!(link_map.links().is_empty());
        assert!(
            !cache_path.exists(),
            "cache file should not be written when no links"
        );
    }

    #[test]
    fn sync_linked_pipelines_sets_field() {
        let mut app = App::new();
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.workflow_groups.push(WorkflowGroup {
            name: "Lint".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        let app = Arc::new(Mutex::new(app));

        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy".into(), "CI".into(), "b".into(), "k".into());

        super::sync_linked_pipelines(&app, &link_map);

        let a = app.lock().unwrap();
        assert_eq!(a.workflow_groups[0].linked_pipeline, Some("deploy".into()));
        assert_eq!(
            a.workflow_groups[1].linked_pipeline, None,
            "unlinked workflow should remain None"
        );
    }

    #[tokio::test]
    async fn full_flow_discovery_apply_health() {
        // Raw YAML with S3 upload
        let aws = LinkMockPipeline {
            names: vec!["deploy-pipe".into()],
            definitions: vec![PipelineDefinition {
                name: "deploy-pipe".into(),
                source_s3: Some(S3Source {
                    bucket: "b".into(),
                    object_key: "art.zip".into(),
                }),
            }],
        };
        let gh = LinkMockActions {
            workflow_yamls: vec![(
                "ci.yml",
                "name: CI\non: push\njobs:\n  b:\n    runs-on: ubuntu-latest\n    steps:\n      - run: aws s3 cp x s3://b/art.zip\n",
            )],
        };

        // Discover links (exercises YAML parsing → S3 extraction → key matching)
        let (mut link_map, _wf) = super::discover_links(&aws, &gh).await;
        assert_eq!(link_map.workflow_for_pipeline("deploy-pipe"), Some("CI"));

        // Setup App with Running states
        let mut app_state = App::new();
        let mut job = Bar::new("build".into());
        job.set_status(BuildStatus::Running);
        app_state.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(100),
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app_state.pipeline_groups.push(PipelineGroup {
            name: "deploy-pipe".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Running,
            pending_link: false,
        });
        app_state.loading_pipelines = false;
        app_state.loading_actions = false;
        let app = Arc::new(Mutex::new(app_state));

        // Sync linked_pipeline field
        super::sync_linked_pipelines(&app, &link_map);
        {
            let a = app.lock().unwrap();
            assert_eq!(
                a.workflow_groups[0].linked_pipeline,
                Some("deploy-pipe".into())
            );
        }

        // Apply links: should suppress GH workflow
        let mut stopped_runs = HashMap::new();
        apply_links(&app, &mut link_map, &mut stopped_runs);
        {
            let a = app.lock().unwrap();
            assert_eq!(
                a.workflow_groups[0].summary_status,
                BuildStatus::Succeeded,
                "linked GH workflow should be suppressed"
            );
        }

        // Health check: all present → not broken
        {
            let mut a = app.lock().unwrap();
            a.check_linkage_health(&link_map);
            assert!(!a.linkage_broken);
        }

        // Simulate pipeline deletion → ghost pipeline → broken
        {
            let mut a = app.lock().unwrap();
            a.pipeline_groups.clear();
            a.check_linkage_health(&link_map);
            assert!(a.linkage_broken, "ghost pipeline should set linkage_broken");
        }
    }

    // --- assign_jobs_to_pipelines tests ---

    use crate::poller::JobS3Info;

    fn make_workflow_file(name: &str, jobs: Vec<JobS3Info>) -> WorkflowFile {
        let s3_uploads: Vec<S3Upload> = jobs.iter().flat_map(|j| j.s3_uploads.clone()).collect();
        WorkflowFile {
            name: name.to_string(),
            s3_uploads,
            jobs,
        }
    }

    fn make_job(id: &str, name: &str, needs: &[&str], s3: &[(&str, &str)]) -> JobS3Info {
        JobS3Info {
            job_id: id.to_string(),
            job_name: name.to_string(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            s3_uploads: s3
                .iter()
                .map(|(b, k)| S3Upload {
                    bucket: b.to_string(),
                    key: k.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn assign_diamond_dependency_pattern() {
        // Mirrors attracr CI: shared deps feed two build jobs
        let wf = make_workflow_file(
            "CI",
            vec![
                make_job("validate-cfn", "Validate CloudFormation", &[], &[]),
                make_job("test-backend", "Backend Tests", &[], &[]),
                make_job("test-frontend", "Frontend Tests", &[], &[]),
                make_job(
                    "build-backend",
                    "Build Backend",
                    &["validate-cfn", "test-backend", "test-frontend"],
                    &[("bucket", "backend/source.zip")],
                ),
                make_job(
                    "build-frontend",
                    "Build Frontend",
                    &["validate-cfn", "test-backend", "test-frontend"],
                    &[("bucket", "frontend/artifacts.zip")],
                ),
            ],
        );

        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "attracr-backend".into(),
            "CI".into(),
            "bucket".into(),
            "backend/source.zip".into(),
        );
        link_map.add_discovered(
            "attracr-frontend".into(),
            "CI".into(),
            "bucket".into(),
            "frontend/artifacts.zip".into(),
        );

        let assignment = assign_jobs_to_pipelines(&[wf], &link_map);

        // Shared: validate-cfn, test-backend, test-frontend (needed by both)
        assert_eq!(assignment.shared_jobs.len(), 1); // one workflow
        let (wf_name, shared_names) = &assignment.shared_jobs[0];
        assert_eq!(wf_name, "CI");
        assert_eq!(shared_names.len(), 3);
        assert!(shared_names.contains(&"Validate CloudFormation".to_string()));
        assert!(shared_names.contains(&"Backend Tests".to_string()));
        assert!(shared_names.contains(&"Frontend Tests".to_string()));

        // Backend pipeline: only Build Backend
        let (_, be_jobs) = &assignment.pipeline_jobs["attracr-backend"];
        assert_eq!(be_jobs, &["Build Backend"]);

        // Frontend pipeline: only Build Frontend
        let (_, fe_jobs) = &assignment.pipeline_jobs["attracr-frontend"];
        assert_eq!(fe_jobs, &["Build Frontend"]);
    }

    #[test]
    fn assign_single_pipeline_no_shared() {
        // One pipeline linked to one workflow — all jobs should be in pipeline, none shared
        let wf = make_workflow_file(
            "Deploy",
            vec![
                make_job("test", "Run Tests", &[], &[]),
                make_job("build", "Build", &["test"], &[("bucket", "app/build.zip")]),
            ],
        );

        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "my-pipeline".into(),
            "Deploy".into(),
            "bucket".into(),
            "app/build.zip".into(),
        );

        let assignment = assign_jobs_to_pipelines(&[wf], &link_map);

        // No shared (only one pipeline uses these jobs)
        assert!(
            assignment.shared_jobs.is_empty()
                || assignment.shared_jobs.iter().all(|(_, j)| j.is_empty())
        );

        // Pipeline gets both jobs in topo order
        let (_, jobs) = &assignment.pipeline_jobs["my-pipeline"];
        assert_eq!(jobs, &["Run Tests", "Build"]);
    }

    #[test]
    fn assign_orphan_jobs_go_to_shared() {
        // Job with no S3 and not in any needs chain → orphan → shared
        let wf = make_workflow_file(
            "CI",
            vec![
                make_job("lint", "Lint", &[], &[]),
                make_job("build", "Build", &[], &[("bucket", "app/build.zip")]),
            ],
        );

        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "my-pipeline".into(),
            "CI".into(),
            "bucket".into(),
            "app/build.zip".into(),
        );

        let assignment = assign_jobs_to_pipelines(&[wf], &link_map);

        // "lint" is orphan (build doesn't need it, it has no S3 upload)
        let shared_names: Vec<&str> = assignment
            .shared_jobs
            .iter()
            .flat_map(|(_, names)| names.iter().map(|s| s.as_str()))
            .collect();
        assert!(shared_names.contains(&"Lint"));

        // "build" goes to pipeline
        let (_, jobs) = &assignment.pipeline_jobs["my-pipeline"];
        assert_eq!(jobs, &["Build"]);
    }

    #[test]
    fn assign_topo_sort_respects_dependency_order() {
        // Chain: a → b → c (c depends on b, b depends on a)
        let wf = make_workflow_file(
            "CI",
            vec![
                make_job("c", "Step C", &["b"], &[("bucket", "out.zip")]),
                make_job("a", "Step A", &[], &[]),
                make_job("b", "Step B", &["a"], &[]),
            ],
        );

        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "pipe".into(),
            "CI".into(),
            "bucket".into(),
            "out.zip".into(),
        );

        let assignment = assign_jobs_to_pipelines(&[wf], &link_map);
        let (_, jobs) = &assignment.pipeline_jobs["pipe"];

        // Should be: A, B, C (dependency order)
        assert_eq!(jobs, &["Step A", "Step B", "Step C"]);
    }

    #[test]
    fn assign_no_workflow_files_returns_empty() {
        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "pipe".into(),
            "CI".into(),
            "bucket".into(),
            "key.zip".into(),
        );

        let assignment = assign_jobs_to_pipelines(&[], &link_map);
        assert!(
            assignment.pipeline_jobs.is_empty() || assignment.pipeline_jobs["pipe"].1.is_empty()
        );
    }

    #[test]
    fn assign_jobs_deep_chain_four_levels() {
        let wf = make_workflow_file(
            "CI",
            vec![
                make_job("d", "Step D", &["c"], &[("bucket", "out.zip")]),
                make_job("b", "Step B", &["a"], &[]),
                make_job("c", "Step C", &["b"], &[]),
                make_job("a", "Step A", &[], &[]),
            ],
        );

        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "pipe".into(),
            "CI".into(),
            "bucket".into(),
            "out.zip".into(),
        );

        let assignment = assign_jobs_to_pipelines(&[wf], &link_map);
        let (_, jobs) = &assignment.pipeline_jobs["pipe"];
        assert_eq!(jobs, &["Step A", "Step B", "Step C", "Step D"]);
    }

    #[test]
    fn assign_jobs_all_jobs_no_s3_all_shared() {
        let wf = make_workflow_file(
            "CI",
            vec![
                make_job("lint", "Lint", &[], &[]),
                make_job("test", "Test", &["lint"], &[]),
                make_job("check", "Check", &[], &[]),
            ],
        );

        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "pipe".into(),
            "CI".into(),
            "bucket".into(),
            "key.zip".into(),
        );

        let assignment = assign_jobs_to_pipelines(&[wf], &link_map);

        let has_pipe_jobs = assignment
            .pipeline_jobs
            .get("pipe")
            .map(|(_, j)| !j.is_empty())
            .unwrap_or(false);
        assert!(!has_pipe_jobs, "no jobs should be assigned to pipeline");

        let shared: Vec<&str> = assignment
            .shared_jobs
            .iter()
            .flat_map(|(_, names)| names.iter().map(|s| s.as_str()))
            .collect();
        assert_eq!(shared.len(), 3);
    }

    #[test]
    fn assign_jobs_multiple_workflows_separate_links() {
        let wf1 = make_workflow_file(
            "Backend CI",
            vec![
                make_job("test-be", "Test Backend", &[], &[]),
                make_job(
                    "build-be",
                    "Build Backend",
                    &["test-be"],
                    &[("bucket", "backend/src.zip")],
                ),
            ],
        );
        let wf2 = make_workflow_file(
            "Frontend CI",
            vec![
                make_job("test-fe", "Test Frontend", &[], &[]),
                make_job(
                    "build-fe",
                    "Build Frontend",
                    &["test-fe"],
                    &[("bucket", "frontend/dist.zip")],
                ),
            ],
        );

        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "backend-pipe".into(),
            "Backend CI".into(),
            "bucket".into(),
            "backend/src.zip".into(),
        );
        link_map.add_discovered(
            "frontend-pipe".into(),
            "Frontend CI".into(),
            "bucket".into(),
            "frontend/dist.zip".into(),
        );

        let assignment = assign_jobs_to_pipelines(&[wf1, wf2], &link_map);

        let (wf, be_jobs) = &assignment.pipeline_jobs["backend-pipe"];
        assert_eq!(wf, "Backend CI");
        assert_eq!(be_jobs, &["Test Backend", "Build Backend"]);

        let (wf, fe_jobs) = &assignment.pipeline_jobs["frontend-pipe"];
        assert_eq!(wf, "Frontend CI");
        assert_eq!(fe_jobs, &["Test Frontend", "Build Frontend"]);
    }

    #[test]
    fn link_map_workflow_for_pipeline_returns_none_when_missing() {
        let link_map = LinkMap::new();
        assert!(link_map.workflow_for_pipeline("nope").is_none());
    }

    #[test]
    fn link_map_workflow_for_pipeline_returns_name() {
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy".into(), "CI".into(), "b".into(), "k".into());
        assert_eq!(link_map.workflow_for_pipeline("deploy"), Some("CI"));
    }

    #[test]
    fn record_completion_deduplicates() {
        let mut map = LinkMap::new();
        map.record_workflow_completion("CI");
        map.record_workflow_completion("CI");
        map.record_workflow_completion("CI");
        assert_eq!(
            map.recent_completions
                .iter()
                .filter(|(n, _)| n == "CI")
                .count(),
            1,
            "should only have one entry for CI"
        );
    }

    #[test]
    fn record_completion_allows_different_workflows() {
        let mut map = LinkMap::new();
        map.record_workflow_completion("CI");
        map.record_workflow_completion("Deploy");
        assert_eq!(map.recent_completions.len(), 2);
    }

    #[test]
    fn pending_link_expires_after_correlation_window() {
        // Setup: linked workflow completed, pipeline idle
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
        let mut stopped = HashMap::new();

        // First call: records completion, sets pending_link = true
        apply_links(&app, &mut link_map, &mut stopped);
        assert!(
            app.lock().unwrap().pipeline_groups[0].pending_link,
            "pending_link should be true right after completion"
        );

        // Simulate repeated calls (as in real polling) — dedup prevents
        // window refresh, so pending_link stays true but doesn't extend
        apply_links(&app, &mut link_map, &mut stopped);
        assert!(
            app.lock().unwrap().pipeline_groups[0].pending_link,
            "pending_link should still be true within window"
        );

        // Expire the completion by replacing with an old timestamp
        link_map.recent_completions.clear();
        link_map
            .recent_completions
            .push(("CI".into(), Instant::now() - Duration::from_secs(60)));

        // Next apply_links: expired completion → pending_link = false
        apply_links(&app, &mut link_map, &mut stopped);
        assert!(
            !app.lock().unwrap().pipeline_groups[0].pending_link,
            "pending_link should be false after correlation window expires"
        );
    }
}
