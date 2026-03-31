# Test Coverage Improvement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add ~35 new tests across unit, multi-poll e2e, and signal/lifecycle layers.

**Architecture:** All tests are inline `#[cfg(test)] mod tests` blocks appended to existing
test modules. No new files. Reuse existing mock infrastructure. TDD: write test, verify fail,
minimal fix if needed (most tests should pass immediately since they test existing code).

**Tech Stack:** Rust, cargo test, existing mock clients (MockPipelineClient, MockActionsClient,
OkAuthClient, ExpiredAuthClient, NetworkErrorAuthClient)

**Parallel execution:** Tasks 1-4 are independent (different files). Task 5 depends on Task 2
(shared mock infra in poller/mod.rs). Task 6 is independent (main.rs).

---

### Task 1: app.rs — clear_warnings_by_prefix unit tests

**Files:**
- Modify: `src/app.rs` (append to `mod tests` block ending at line 472)

- [ ] **Step 1: Write 4 failing tests**

Append before the closing `}` of the test module at end of file:

```rust
    #[test]
    fn clear_warnings_by_prefix_removes_matching() {
        let mut app = App::new();
        app.push_warning("AWS: SSO expired".to_string());
        app.push_warning("AWS: timeout".to_string());
        app.clear_warnings_by_prefix("AWS:");
        assert!(app.warnings.is_empty());
    }

    #[test]
    fn clear_warnings_by_prefix_keeps_others() {
        let mut app = App::new();
        app.push_warning("AWS: SSO expired".to_string());
        app.push_warning("GitHub: rate limited".to_string());
        app.clear_warnings_by_prefix("AWS:");
        assert_eq!(app.warnings.len(), 1);
        assert_eq!(app.warnings[0], "GitHub: rate limited");
    }

    #[test]
    fn clear_warnings_by_prefix_no_match_noop() {
        let mut app = App::new();
        app.push_warning("GitHub: rate limited".to_string());
        app.clear_warnings_by_prefix("AWS:");
        assert_eq!(app.warnings.len(), 1);
    }

    #[test]
    fn clear_warnings_by_prefix_empty_warnings() {
        let mut app = App::new();
        app.clear_warnings_by_prefix("AWS:");
        assert!(app.warnings.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib app::tests::clear_warnings -- --nocapture`
Expected: all 4 PASS (testing existing working code)

- [ ] **Step 3: Commit**

```bash
cargo fmt && cargo clippy
git add src/app.rs
git commit -m "test: clear_warnings_by_prefix unit tests"
```

---

### Task 2: poller/mod.rs — stage_status_and_timestamp + update_workflow_jobs unit tests

**Files:**
- Modify: `src/poller/mod.rs` (append to `mod tests` block ending at line 1621)

- [ ] **Step 1: Write 4 failing tests**

Append before the closing `}` of the test module:

```rust
    #[test]
    fn stage_status_and_timestamp_empty_actions() {
        let actions: Vec<ActionState> = vec![];
        let (status, timestamp) = stage_status_and_timestamp(&actions);
        assert_eq!(status, BuildStatus::Idle);
        assert!(timestamp.is_none());
    }

    #[test]
    fn stage_status_and_timestamp_failed_with_time() {
        use chrono::TimeZone;
        let ts = chrono::Utc.with_ymd_and_hms(2026, 3, 18, 14, 25, 0).unwrap();
        let actions = vec![ActionState {
            status: BuildStatus::Failed,
            last_status_change: Some(ts),
        }];
        let (status, timestamp) = stage_status_and_timestamp(&actions);
        assert_eq!(status, BuildStatus::Failed);
        assert_eq!(timestamp, Some(ts));
    }

    #[test]
    fn stage_status_and_timestamp_running_ignores_timestamp() {
        use chrono::TimeZone;
        let ts = chrono::Utc.with_ymd_and_hms(2026, 3, 18, 14, 25, 0).unwrap();
        let actions = vec![ActionState {
            status: BuildStatus::Running,
            last_status_change: Some(ts),
        }];
        let (status, timestamp) = stage_status_and_timestamp(&actions);
        assert_eq!(status, BuildStatus::Running);
        assert!(timestamp.is_none(), "Running should not expose timestamp");
    }

    #[test]
    fn stage_status_and_timestamp_multi_action_uses_last() {
        use chrono::TimeZone;
        let ts = chrono::Utc.with_ymd_and_hms(2026, 3, 18, 15, 0, 0).unwrap();
        let actions = vec![
            ActionState {
                status: BuildStatus::Succeeded,
                last_status_change: Some(
                    chrono::Utc.with_ymd_and_hms(2026, 3, 18, 14, 0, 0).unwrap(),
                ),
            },
            ActionState {
                status: BuildStatus::Failed,
                last_status_change: Some(ts),
            },
        ];
        let (status, timestamp) = stage_status_and_timestamp(&actions);
        assert_eq!(status, BuildStatus::Failed, "should use last action's status");
        assert_eq!(timestamp, Some(ts), "should use last action's timestamp");
    }

    #[tokio::test]
    async fn update_workflow_jobs_missing_group_is_noop() {
        let mut app = App::new();
        // No workflow groups exist — should not panic
        update_workflow_jobs(&mut app, "nonexistent", vec![
            JobInfo {
                name: "build".to_string(),
                status: BuildStatus::Running,
                completed_at: None,
            },
        ]);
        assert!(app.workflow_groups.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib poller::tests::stage_status_and_timestamp -- --nocapture`
Run: `cargo test --lib poller::tests::update_workflow_jobs_missing -- --nocapture`
Expected: all 5 PASS

- [ ] **Step 3: Commit**

```bash
cargo fmt && cargo clippy
git add src/poller/mod.rs
git commit -m "test: stage_status_and_timestamp edge cases + update_workflow_jobs noop"
```

---

### Task 3: linkage.rs — topo_sort and assign_jobs edge case tests

**Files:**
- Modify: `src/linkage.rs` (append to `mod tests` block ending at line 2246)

- [ ] **Step 1: Write 5 tests**

Append before the closing `}` of the test module:

```rust
    #[test]
    fn assign_jobs_deep_chain_four_levels() {
        // A → B → C → D (linear chain, verify topo order preserved)
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
        // No jobs have S3 uploads — all should be shared/orphan
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

        // Pipeline should have no jobs (none upload to its S3 key)
        let has_pipe_jobs = assignment
            .pipeline_jobs
            .get("pipe")
            .map(|(_, j)| !j.is_empty())
            .unwrap_or(false);
        assert!(!has_pipe_jobs, "no jobs should be assigned to pipeline");

        // All jobs should be shared
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
        link_map.add_discovered(
            "deploy".into(),
            "CI".into(),
            "b".into(),
            "k".into(),
        );
        assert_eq!(link_map.workflow_for_pipeline("deploy"), Some("CI"));
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib linkage::tests::assign_jobs_deep -- --nocapture`
Run: `cargo test --lib linkage::tests::assign_jobs_all_jobs -- --nocapture`
Run: `cargo test --lib linkage::tests::assign_jobs_multiple_workflows -- --nocapture`
Run: `cargo test --lib linkage::tests::link_map_workflow -- --nocapture`
Expected: all 5 PASS

- [ ] **Step 3: Commit**

```bash
cargo fmt && cargo clippy
git add src/linkage.rs
git commit -m "test: linkage topo_sort edge cases + multi-workflow assignment"
```

---

### Task 4: ui/mod.rs — build_pipeline_centric_sections unit tests

**Files:**
- Modify: `src/ui/mod.rs` (append to `mod tests` block ending at line 1825)

- [ ] **Step 1: Write 5 tests**

Append before the closing `}` of the test module. These need `use crate::linkage::JobAssignment;`
and `use std::collections::HashMap;` at the top of the test block (check if already imported).

```rust
    #[test]
    fn build_pipeline_centric_empty_assignment() {
        let assignment = JobAssignment {
            pipeline_jobs: HashMap::new(),
            shared_jobs: vec![],
        };
        let app = App::new();
        let sections = build_pipeline_centric_sections(&assignment, &app);
        assert!(sections.is_empty());
    }

    #[test]
    fn build_pipeline_centric_shared_section_first() {
        let assignment = JobAssignment {
            pipeline_jobs: HashMap::new(),
            shared_jobs: vec![("CI".into(), vec!["Lint".into(), "Test".into()])],
        };
        let app = App::new();
        let sections = build_pipeline_centric_sections(&assignment, &app);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].title, " CI ");
        assert_eq!(sections[0].rows.len(), 2);
    }

    #[test]
    fn build_pipeline_centric_skips_empty_shared() {
        let assignment = JobAssignment {
            pipeline_jobs: HashMap::new(),
            shared_jobs: vec![("CI".into(), vec![])],
        };
        let app = App::new();
        let sections = build_pipeline_centric_sections(&assignment, &app);
        assert!(sections.is_empty(), "empty shared_names should be skipped");
    }

    #[test]
    fn build_pipeline_centric_pipeline_section_with_stages() {
        let mut pipeline_jobs = HashMap::new();
        pipeline_jobs.insert(
            "deploy".to_string(),
            ("CI".to_string(), vec!["Build".to_string()]),
        );
        let assignment = JobAssignment {
            pipeline_jobs,
            shared_jobs: vec![],
        };

        let mut app = App::new();
        let mut stage = Bar::new("Source".into());
        stage.set_status(BuildStatus::Succeeded);
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![stage],
            gone: false,
            summary_status: BuildStatus::Succeeded,
            pending_link: false,
        });

        let sections = build_pipeline_centric_sections(&assignment, &app);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].title, " deploy ");
        // Should have: 1 job bar + 1 pipeline header + 1 stage bar = 3 rows
        assert_eq!(sections[0].rows.len(), 3);
    }

    #[test]
    fn build_pipeline_centric_reviews_section_last() {
        let assignment = JobAssignment {
            pipeline_jobs: HashMap::new(),
            shared_jobs: vec![("CI".into(), vec!["Test".into()])],
        };

        let mut app = App::new();
        let mut review_job = Bar::new("review-check".into());
        review_job.set_status(BuildStatus::Running);
        app.workflow_groups.push(WorkflowGroup {
            name: "Review".into(),
            jobs: vec![review_job],
            gone: false,
            summary_status: BuildStatus::Running,
            run_id: None,
            category: WorkflowCategory::Review,
            linked_pipeline: None,
        });

        let sections = build_pipeline_centric_sections(&assignment, &app);
        assert_eq!(sections.len(), 2); // shared + reviews
        assert_eq!(sections[1].title, " reviews ");
        assert_eq!(sections[1].rows.len(), 1);
    }
```

- [ ] **Step 2: Add necessary imports to test module**

Check if `JobAssignment` and `HashMap` are already imported in the test module. If not, add
to the top of the `mod tests` block:

```rust
    use crate::linkage::JobAssignment;
    use std::collections::HashMap;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib ui::tests::build_pipeline_centric -- --nocapture`
Expected: all 5 PASS

- [ ] **Step 4: Commit**

```bash
cargo fmt && cargo clippy
git add src/ui/mod.rs
git commit -m "test: build_pipeline_centric_sections unit tests"
```

---

### Task 5: poller/mod.rs — multi-poll e2e scenarios

**Files:**
- Modify: `src/poller/mod.rs` (append to `mod tests` block ending at line 1621)

**Depends on:** Task 2 (appends to same file, coordinate ordering)

- [ ] **Step 1: Write 10 e2e tests**

Append before the closing `}` of the test module:

```rust
    // === Multi-poll E2E scenarios ===

    #[tokio::test]
    async fn e2e_pipeline_lifecycle_idle_running_succeeded() {
        let app = Arc::new(Mutex::new(App::new()));
        let actions = MockActionsClient { runs: vec![] };

        // Poll 1: pipeline arrives Idle
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Idle, vec![
                mock_stage("Source", vec![("checkout", BuildStatus::Idle)]),
            ])],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Idle);
            assert!(!a.has_any_running());
        }

        // Poll 2: pipeline now Running
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Running, vec![
                mock_stage("Source", vec![("checkout", BuildStatus::Running)]),
            ])],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Running);
            assert!(a.has_any_running());
        }

        // Poll 3: pipeline Succeeded
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Succeeded, vec![
                mock_stage("Source", vec![("checkout", BuildStatus::Succeeded)]),
            ])],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Succeeded);
            assert!(!a.has_any_running());
        }
    }

    #[tokio::test]
    async fn e2e_pipeline_lifecycle_running_to_failed() {
        let app = Arc::new(Mutex::new(App::new()));
        let actions = MockActionsClient { runs: vec![] };

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Running, vec![
                mock_stage("Build", vec![("compile", BuildStatus::Running)]),
            ])],
        };
        poll_once(&app, &pipes, &actions).await;
        assert!(app.lock().unwrap().has_any_running());

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Failed, vec![
                mock_stage("Build", vec![("compile", BuildStatus::Failed)]),
            ])],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups[0].summary_status, BuildStatus::Failed);
            assert!(!a.has_any_running());
        }
    }

    #[tokio::test]
    async fn e2e_workflow_appears_then_goes_gone() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };

        // Poll 1: CI workflow present
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![],
            }],
        };
        poll_once(&app, &pipes, &actions).await;
        assert_eq!(app.lock().unwrap().workflow_groups.len(), 1);
        assert!(!app.lock().unwrap().workflow_groups[0].gone);

        // Poll 2: CI disappears from API
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.workflow_groups.len(), 1, "group should persist");
            assert!(a.workflow_groups[0].gone, "should be marked gone");
        }
    }

    #[tokio::test]
    async fn e2e_workflow_reappears_after_gone() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };

        // Poll 1: present
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![],
            }],
        };
        poll_once(&app, &pipes, &actions).await;

        // Poll 2: gone
        let actions = MockActionsClient { runs: vec![] };
        poll_once(&app, &pipes, &actions).await;
        assert!(app.lock().unwrap().workflow_groups[0].gone);

        // Poll 3: reappears
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 2,
                status: BuildStatus::Running,
                jobs: vec![],
            }],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert!(!a.workflow_groups[0].gone, "should no longer be gone");
            assert_eq!(a.workflow_groups[0].summary_status, BuildStatus::Running);
        }
    }

    #[tokio::test]
    async fn e2e_auth_recovery_flow() {
        let app = Arc::new(Mutex::new(App::new()));

        // Poll 1: AWS returns expired token
        let pipes = ExpiredTokenClient;
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;
        {
            let a = app.lock().unwrap();
            assert!(matches!(a.aws_health, SourceHealth::AuthFailed { .. }));
            assert!(!a.warnings.is_empty());
        }

        // Poll 2: check_aws_auth recovers
        let auth_client = OkAuthClient;
        check_aws_auth(&app, &auth_client, "my-profile").await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.aws_health, SourceHealth::Healthy);
            assert!(a.warnings.is_empty());
        }

        // Poll 3: normal pipeline poll succeeds
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Succeeded, vec![])],
        };
        poll_pipelines_tick(&app, &pipes, "my-profile", false).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.aws_health, SourceHealth::Healthy);
            assert_eq!(a.pipeline_groups.len(), 1);
        }
    }

    #[tokio::test]
    async fn e2e_rate_limit_backoff_and_recovery() {
        let app = Arc::new(Mutex::new(App::new()));

        // Poll 1: GH returns 403
        let client = RateLimitActionsClient {
            error_msg: "403 Forbidden".to_string(),
        };
        poll_actions_tick(&app, &client).await;
        {
            let a = app.lock().unwrap();
            assert!(a.rate_limited_until.is_some());
        }

        // Poll 2: still within backoff window — skip path
        let client = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![],
            }],
        };
        poll_actions_tick(&app, &client).await;
        {
            let a = app.lock().unwrap();
            assert!(
                a.warnings.iter().any(|w| w.contains("rate-limited")),
                "should show rate-limited warning"
            );
            // Workflows should not have been updated (skip path)
            assert!(a.workflow_groups.is_empty());
        }

        // Expire the backoff
        {
            let mut a = app.lock().unwrap();
            a.rate_limited_until = None;
            a.warnings.clear();
        }

        // Poll 3: recovery
        poll_actions_tick(&app, &client).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.workflow_groups.len(), 1);
            assert_eq!(a.workflow_groups[0].name, "CI");
        }
    }

    #[tokio::test]
    async fn e2e_multi_pipeline_independent_lifecycle() {
        let app = Arc::new(Mutex::new(App::new()));
        let actions = MockActionsClient { runs: vec![] };

        // Poll 1: two pipelines, one Running, one Succeeded
        let pipes = MockPipelineClient {
            pipelines: vec![
                mock_pipeline("pipe-a", BuildStatus::Running, vec![
                    mock_stage("Build", vec![("compile", BuildStatus::Running)]),
                ]),
                mock_pipeline("pipe-b", BuildStatus::Succeeded, vec![
                    mock_stage("Deploy", vec![("deploy", BuildStatus::Succeeded)]),
                ]),
            ],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert_eq!(a.pipeline_groups.len(), 2);
            let pa = a.pipeline_groups.iter().find(|g| g.name == "pipe-a").unwrap();
            assert_eq!(pa.summary_status, BuildStatus::Running);
            let pb = a.pipeline_groups.iter().find(|g| g.name == "pipe-b").unwrap();
            assert_eq!(pb.summary_status, BuildStatus::Succeeded);
            assert!(a.has_any_running());
        }

        // Poll 2: pipe-a completes, pipe-b unchanged
        let pipes = MockPipelineClient {
            pipelines: vec![
                mock_pipeline("pipe-a", BuildStatus::Succeeded, vec![
                    mock_stage("Build", vec![("compile", BuildStatus::Succeeded)]),
                ]),
                mock_pipeline("pipe-b", BuildStatus::Succeeded, vec![
                    mock_stage("Deploy", vec![("deploy", BuildStatus::Succeeded)]),
                ]),
            ],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let a = app.lock().unwrap();
            assert!(!a.has_any_running());
        }
    }

    #[tokio::test]
    async fn e2e_classify_workflows_across_polls() {
        let app = Arc::new(Mutex::new(App::new()));
        let pipes = MockPipelineClient { pipelines: vec![] };
        let config = Config::try_from_args(&[
            "cibars", "--aws-profile", "p", "--region", "r", "--github-repo", "o/r",
        ]).unwrap();

        // Poll 1: CI workflow
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Running,
                jobs: vec![],
            }],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let mut a = app.lock().unwrap();
            classify_workflows(&mut a, &config);
            assert_eq!(a.workflow_groups[0].category, WorkflowCategory::CI);
        }

        // Poll 2: new Review workflow appears
        let actions = MockActionsClient {
            runs: vec![
                WorkflowRunInfo {
                    workflow_name: "CI".into(),
                    run_id: 2,
                    status: BuildStatus::Succeeded,
                    jobs: vec![],
                },
                WorkflowRunInfo {
                    workflow_name: "Claude Code Review".into(),
                    run_id: 3,
                    status: BuildStatus::Running,
                    jobs: vec![],
                },
            ],
        };
        poll_once(&app, &pipes, &actions).await;
        {
            let mut a = app.lock().unwrap();
            classify_workflows(&mut a, &config);
            let ci = a.workflow_groups.iter().find(|g| g.name == "CI").unwrap();
            assert_eq!(ci.category, WorkflowCategory::CI);
            let review = a
                .workflow_groups
                .iter()
                .find(|g| g.name == "Claude Code Review")
                .unwrap();
            assert_eq!(review.category, WorkflowCategory::Review);
        }
    }

    #[tokio::test]
    async fn e2e_concurrent_pipeline_and_workflow_updates() {
        let app = Arc::new(Mutex::new(App::new()));

        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Running, vec![])],
        };
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![JobInfo {
                    name: "build".into(),
                    status: BuildStatus::Succeeded,
                    completed_at: None,
                }],
            }],
        };
        poll_once(&app, &pipes, &actions).await;

        let a = app.lock().unwrap();
        assert_eq!(a.pipeline_groups.len(), 1);
        assert_eq!(a.pipeline_groups[0].name, "deploy");
        assert_eq!(a.workflow_groups.len(), 1);
        assert_eq!(a.workflow_groups[0].name, "CI");
        assert_eq!(a.workflow_groups[0].jobs.len(), 1);
        // Both updated independently without interference
        assert_eq!(
            a.pipeline_groups[0].summary_status,
            BuildStatus::Running
        );
        assert_eq!(
            a.workflow_groups[0].summary_status,
            BuildStatus::Succeeded
        );
    }

    #[tokio::test]
    async fn e2e_linkage_gh_completes_cp_starts() {
        use crate::linkage::{apply_links, LinkMap};

        let app = Arc::new(Mutex::new(App::new()));
        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "deploy".into(),
            "CI".into(),
            "bucket".into(),
            "key.zip".into(),
        );

        // Poll 1: GH Running, CP Idle
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Idle, vec![])],
        };
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Running,
                jobs: vec![JobInfo {
                    name: "build".into(),
                    status: BuildStatus::Running,
                    completed_at: None,
                }],
            }],
        };
        poll_once(&app, &pipes, &actions).await;

        // Poll 2: GH Succeeded, CP now Running
        let pipes = MockPipelineClient {
            pipelines: vec![mock_pipeline("deploy", BuildStatus::Running, vec![
                mock_stage("Source", vec![("s3", BuildStatus::Succeeded)]),
            ])],
        };
        let actions = MockActionsClient {
            runs: vec![WorkflowRunInfo {
                workflow_name: "CI".into(),
                run_id: 1,
                status: BuildStatus::Succeeded,
                jobs: vec![JobInfo {
                    name: "build".into(),
                    status: BuildStatus::Running,
                    completed_at: None,
                }],
            }],
        };
        poll_once(&app, &pipes, &actions).await;

        // Apply linkage: linked GH jobs should be marked Succeeded
        let mut stopped_runs = std::collections::HashMap::new();
        apply_links(&app, &mut link_map, &mut stopped_runs);

        let a = app.lock().unwrap();
        let ci = a.workflow_groups.iter().find(|g| g.name == "CI").unwrap();
        let build_job = ci.jobs.iter().find(|j| j.name == "build").unwrap();
        assert_eq!(
            build_job.status,
            BuildStatus::Succeeded,
            "linked GH job should be Succeeded when CP is Running"
        );
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib poller::tests::e2e_ -- --nocapture`
Expected: all 10 PASS

- [ ] **Step 3: Commit**

```bash
cargo fmt && cargo clippy
git add src/poller/mod.rs
git commit -m "test: multi-poll e2e scenarios for pipeline lifecycle, auth, linkage"
```

---

### Task 6: main.rs — signal/lifecycle e2e tests

**Files:**
- Modify: `src/main.rs` (append to `mod tests` block ending at line 497)

- [ ] **Step 1: Write 5 e2e tests**

Append before the closing `}` of the test module:

```rust
    #[test]
    fn e2e_double_boost_stays_watching() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();
        assert_eq!(scheduler.state(), PollState::Idle);

        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert_eq!(scheduler.state(), PollState::Watching);

        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert_eq!(scheduler.state(), PollState::Watching);
        assert!(
            app.lock().unwrap().push_signal_at.is_none(),
            "boost should never set push_signal_at"
        );
    }

    #[test]
    fn e2e_sigusr1_then_boost_both_watching() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();

        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Sigusr1);
        assert_eq!(scheduler.state(), PollState::Watching);
        assert!(app.lock().unwrap().push_signal_at.is_some());

        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert_eq!(scheduler.state(), PollState::Watching);
    }

    #[test]
    fn e2e_boost_never_sets_push_signal_across_states() {
        let app = Arc::new(Mutex::new(App::new()));
        let mut scheduler = PollScheduler::new();

        // Boost from Idle
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert!(app.lock().unwrap().push_signal_at.is_none());

        // Boost again from Watching
        handle_poll_interrupt(&app, &mut scheduler, InterruptSource::Boost);
        assert!(app.lock().unwrap().push_signal_at.is_none());
    }

    #[test]
    fn e2e_pid_file_overwritten_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("cibars.pid");

        // Write initial PID file with fake PID
        std::fs::write(&pid_path, "99999").unwrap();

        // Overwrite with current PID (simulates restart)
        write_pid_file(&pid_path).unwrap();
        let content = std::fs::read_to_string(&pid_path).unwrap();
        assert_eq!(content, std::process::id().to_string());
    }

    #[test]
    fn e2e_pid_file_creates_nested_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("deep").join("nested").join("cibars.pid");

        // Parent dirs don't exist yet
        assert!(!pid_path.parent().unwrap().exists());

        write_pid_file(&pid_path).unwrap();
        assert!(pid_path.exists());
        let content = std::fs::read_to_string(&pid_path).unwrap();
        assert_eq!(content, std::process::id().to_string());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib tests::e2e_ -- --nocapture`
Expected: all 5 PASS

- [ ] **Step 3: Commit**

```bash
cargo fmt && cargo clippy
git add src/main.rs
git commit -m "test: signal/lifecycle e2e — double boost, PID overwrite, nested dirs"
```

---

### Task 7: Final verification

- [ ] **Step 1: Run full test suite**

```bash
cargo test 2>&1 | tail -5
```

Expected: ~542 tests passed, 0 failed

- [ ] **Step 2: Run clippy and fmt**

```bash
cargo clippy 2>&1 | tail -5
cargo fmt -- --check
```

Expected: no warnings, no formatting changes

- [ ] **Step 3: Push to master**

```bash
git push origin master
```
