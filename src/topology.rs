use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Bar, BuildStatus, PipelineGroup, WorkflowGroup};
use crate::{app::App, config::Config};

#[derive(Debug, Serialize, Deserialize)]
pub struct TopologyCache {
    schema_version: u32,
    github: Option<GithubTopology>,
    aws: Option<AwsTopology>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GithubTopology {
    repo: String,
    branch: Option<String>,
    workflows: Vec<GroupTopology>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AwsTopology {
    profile: String,
    region: String,
    pipelines: Vec<GroupTopology>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GroupTopology {
    name: String,
    bars: Vec<String>,
}

impl TopologyCache {
    pub fn matches_link_scope(&self, config: &Config) -> bool {
        matches!((&self.github, &config.github, &self.aws, &config.aws),
            (Some(gh), Some(current_gh), Some(aws), Some(current_aws))
            if gh.repo == current_gh.repo && gh.branch == current_gh.branch
            && aws.profile == current_aws.profile && aws.region == current_aws.region)
    }
    pub fn capture(app: &App, config: &Config) -> Self {
        Self {
            schema_version: 1,
            github: config.github.as_ref().map(|source| GithubTopology {
                repo: source.repo.clone(),
                branch: source.branch.clone(),
                workflows: app
                    .workflow_groups
                    .iter()
                    .filter(|g| !g.gone)
                    .map(|group| GroupTopology {
                        name: group.name.clone(),
                        bars: group
                            .jobs
                            .iter()
                            .filter(|b| !b.gone)
                            .map(|b| b.name.clone())
                            .collect(),
                    })
                    .collect(),
            }),
            // Assumption: profile/region identify a source before credentials load.
            // Only neutral names survive a profile remap; no account execution state does.
            aws: config.aws.as_ref().map(|source| AwsTopology {
                profile: source.profile.clone(),
                region: source.region.clone(),
                pipelines: app
                    .pipeline_groups
                    .iter()
                    .filter(|g| !g.gone)
                    .map(|group| GroupTopology {
                        name: group.name.clone(),
                        bars: group
                            .stages
                            .iter()
                            .filter(|b| !b.gone)
                            .map(|b| b.name.clone())
                            .collect(),
                    })
                    .collect(),
            }),
        }
    }

    pub fn hydrate(&self, app: &mut App, config: &Config) {
        if let (Some(cached), Some(source)) = (&self.github, &config.github) {
            if app.actions_enabled && cached.repo == source.repo && cached.branch == source.branch {
                app.workflow_groups = cached
                    .workflows
                    .iter()
                    .map(|group| WorkflowGroup {
                        name: group.name.clone(),
                        jobs: group.bars.iter().cloned().map(Bar::new).collect(),
                        gone: false,
                        summary_status: BuildStatus::Idle,
                        run_id: None,
                        category: config.classify_workflow(&group.name),
                        linked_pipeline: None,
                    })
                    .collect();
            }
        }
        if let (Some(cached), Some(source)) = (&self.aws, &config.aws) {
            if app.pipelines_enabled
                && cached.profile == source.profile
                && cached.region == source.region
            {
                app.pipeline_groups = cached
                    .pipelines
                    .iter()
                    .map(|group| PipelineGroup {
                        name: group.name.clone(),
                        stages: group.bars.iter().cloned().map(Bar::new).collect(),
                        gone: false,
                        summary_status: BuildStatus::Idle,
                        pending_link: false,
                    })
                    .collect();
            }
        }
    }
}

pub fn load_topology(path: &Path) -> Result<Option<TopologyCache>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed to read topology cache"),
    };
    let cache: TopologyCache =
        toml::from_str(&contents).context("failed to parse topology cache")?;
    Ok((cache.schema_version == 1).then_some(cache))
}

pub fn save_topology(path: &Path, cache: &TopologyCache) -> Result<()> {
    let contents = toml::to_string_pretty(cache).context("failed to serialize topology cache")?;
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == contents => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to read previous topology cache"),
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contents).context("failed to write topology cache temporary file")?;
    std::fs::rename(&tmp, path).context("failed to replace topology cache")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AwsSource, GithubSource};
    use crate::model::{Bar, BuildStatus, PipelineGroup, WorkflowCategory, WorkflowGroup};

    fn config() -> Config {
        Config {
            github: Some(GithubSource {
                repo: "org/repo".into(),
                branch: Some("master".into()),
            }),
            aws: Some(AwsSource {
                profile: "readonly".into(),
                region: "eu-west-1".into(),
            }),
            review_workflows: vec![],
        }
    }

    fn live_app() -> App {
        let mut app = App::new();
        let mut job = Bar::new("Tests".into());
        job.set_status(BuildStatus::Running);
        job.tick(10);
        job.last_finished = Some(chrono::Utc::now());
        let mut gone = Bar::new("Removed".into());
        gone.gone = true;
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![job.clone(), Bar::new("Build".into()), gone.clone()],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: Some(123),
            category: WorkflowCategory::CI,
            linked_pipeline: Some("deploy".into()),
        });
        let mut old = app.workflow_groups[0].clone();
        old.name = "Old CI".into();
        old.gone = true;
        app.workflow_groups.push(old);
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![job, Bar::new("Deploy".into()), gone],
            gone: false,
            summary_status: BuildStatus::Failed,
            pending_link: true,
        });
        let mut old = app.pipeline_groups[0].clone();
        old.gone = true;
        app.pipeline_groups.push(old);
        app
    }

    #[test]
    fn topology_round_trip_preserves_names_order_without_execution_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".cibars-topology.toml");
        save_topology(&path, &TopologyCache::capture(&live_app(), &config())).unwrap();
        let cache = load_topology(&path).unwrap().expect("saved topology");
        let mut restored = App::new();
        cache.hydrate(&mut restored, &config());
        assert_eq!(restored.workflow_groups.len(), 1);
        assert_eq!(restored.workflow_groups[0].name, "CI");
        assert_eq!(
            restored.workflow_groups[0]
                .jobs
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            ["Tests", "Build"]
        );
        assert_eq!(restored.pipeline_groups.len(), 1);
        assert_eq!(restored.pipeline_groups[0].name, "deploy");
        assert_eq!(
            restored.pipeline_groups[0]
                .stages
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            ["Tests", "Deploy"]
        );
        let text = std::fs::read_to_string(path).unwrap();
        for execution_field in [
            "status",
            "run_id",
            "finished",
            "fill",
            "pending_link",
            "linked_pipeline",
        ] {
            assert!(
                !text.contains(execution_field),
                "execution field persisted: {execution_field}"
            );
        }
    }

    #[test]
    fn topology_hydrates_neutral_offline_and_classifies_current_config() {
        let cache = TopologyCache::capture(&live_app(), &config());
        let mut current = config();
        current.review_workflows.push("CI".into());
        let mut app = App::with_sources(true, true);
        cache.hydrate(&mut app, &current);
        assert_eq!(app.workflow_groups.len(), 1);
        let workflow = &app.workflow_groups[0];
        assert_eq!(workflow.category, WorkflowCategory::Review);
        assert_eq!(workflow.summary_status, BuildStatus::Idle);
        assert_eq!(workflow.run_id, None);
        assert_eq!(workflow.linked_pipeline, None);
        assert_eq!(app.pipeline_groups[0].summary_status, BuildStatus::Idle);
        assert!(!app.pipeline_groups[0].pending_link);
        for bar in workflow.jobs.iter().chain(&app.pipeline_groups[0].stages) {
            assert_eq!(bar.status, BuildStatus::Idle);
            assert_eq!((bar.fill, bar.write_pos, bar.laps), (0, 0, 0));
            assert_eq!(bar.last_finished, None);
        }
        assert!(!app.has_any_running());
        assert!(app.last_poll.is_none());
        assert!(app.loading_actions && app.loading_pipelines);
    }

    #[test]
    fn topology_scopes_sources_independently() {
        let cache = TopologyCache::capture(&live_app(), &config());
        for mismatch in ["repo", "branch", "profile", "region"] {
            let mut current = config();
            match mismatch {
                "repo" => current.github.as_mut().unwrap().repo = "other/repo".into(),
                "branch" => current.github.as_mut().unwrap().branch = None,
                "profile" => current.aws.as_mut().unwrap().profile = "other".into(),
                _ => current.aws.as_mut().unwrap().region = "us-east-1".into(),
            }
            let mut app = App::new();
            cache.hydrate(&mut app, &current);
            assert_eq!(
                app.workflow_groups.len(),
                usize::from(matches!(mismatch, "profile" | "region")),
                "{mismatch}"
            );
            assert_eq!(
                app.pipeline_groups.len(),
                usize::from(matches!(mismatch, "repo" | "branch")),
                "{mismatch}"
            );
        }
    }

    #[test]
    fn topology_disabled_sources_never_restore_rows() {
        let cache = TopologyCache::capture(&live_app(), &config());
        for github_enabled in [true, false] {
            let mut current = config();
            if github_enabled {
                current.aws = None;
            } else {
                current.github = None;
            }
            let mut app = App::with_sources(github_enabled, !github_enabled);
            cache.hydrate(&mut app, &current);
            assert_eq!(app.workflow_groups.len(), usize::from(github_enabled));
            assert_eq!(app.pipeline_groups.len(), usize::from(!github_enabled));
        }
    }

    #[test]
    fn topology_empty_inventory_replaces_previous_structure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.toml");
        save_topology(&path, &TopologyCache::capture(&live_app(), &config())).unwrap();
        save_topology(&path, &TopologyCache::capture(&App::new(), &config())).unwrap();
        let mut restored = App::new();
        load_topology(&path)
            .unwrap()
            .expect("empty snapshot is valid")
            .hydrate(&mut restored, &config());
        assert!(restored.workflow_groups.is_empty());
        assert!(restored.pipeline_groups.is_empty());
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn topology_missing_unknown_schema_and_corrupt_files_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.toml");
        assert!(load_topology(&path).unwrap().is_none());
        std::fs::write(&path, "schema_version = 999\n").unwrap();
        assert!(load_topology(&path).unwrap().is_none());
        std::fs::write(&path, "not toml [").unwrap();
        assert!(load_topology(&path).is_err());
    }

    #[test]
    fn topology_unchanged_save_preserves_existing_file_and_retained_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.toml");
        let app = live_app();
        save_topology(&path, &TopologyCache::capture(&app, &config())).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        // A failed refresh leaves the prior app structure intact.
        save_topology(&path, &TopologyCache::capture(&app, &config())).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        let mut restored = App::new();
        load_topology(&path)
            .unwrap()
            .unwrap()
            .hydrate(&mut restored, &config());
        assert_eq!(restored.workflow_groups[0].jobs[0].name, "Tests");
    }
}
