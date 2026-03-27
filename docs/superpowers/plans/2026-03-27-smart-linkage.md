# Smart Linkage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist discovered GH↔CP links to a `.cibars-links.toml` cache, add `'l'` key
for rediscovery, detect broken linkage, and render linked pipelines indented under their
parent workflow in the Actions block.

**Architecture:** Cache-first startup loads links from TOML; falls back to API discovery.
A `link_notify: Arc<Notify>` (mirroring boost pattern) lets the UI's `'l'` key trigger
re-discovery in the async orchestrator. Linked pipelines are shown indented under their
workflow in the Actions block; the CodePipelines block only shows unlinked pipelines.

**Tech Stack:** Rust, ratatui, tokio, serde/toml (all already in Cargo.toml)

**Spec:** `docs/superpowers/specs/2026-03-27-smart-linkage-design.md`

---

## File Map

| File | Responsibility | Action |
|---|---|---|
| `src/model.rs` | Add `linked_pipeline: Option<String>` to `WorkflowGroup` | Modify |
| `src/linkage.rs` | Cache structs, LinkMap methods, load/save fns | Modify |
| `src/app.rs` | `linkage_broken`, `linkage_discovering` fields + health check | Modify |
| `src/main.rs` | `link_notify`, cache-first startup, `run_discovery` helper, orchestrator wiring | Modify |
| `src/ui/mod.rs` | `'l'` key, tree rendering, filtered pipeline sort | Modify |
| `src/ui/statusbar.rs` | `l=relink` indicator with broken/discovering states | Modify |

---

### Task 1: Add `linked_pipeline` field to `WorkflowGroup`

**Files:**
- Modify: `src/model.rs:115-125` (struct definition)
- Modify: ~50 construction sites across `src/model.rs`, `src/app.rs`, `src/linkage.rs`,
  `src/poller/mod.rs`, `src/ui/bar.rs`, `src/ui/mod.rs`

This is purely mechanical — add the field and `linked_pipeline: None` to every struct
literal. No new tests needed (existing tests validate after the field is added).

- [ ] **Step 1: Add field to `WorkflowGroup` struct**

In `src/model.rs`, add `linked_pipeline` after `category`:

```rust
pub struct WorkflowGroup {
    pub name: String,
    pub jobs: Vec<Bar>,
    pub gone: bool,
    /// Workflow-level status from runs API; shown as dot before jobs load.
    pub summary_status: BuildStatus,
    /// The GH Actions run_id currently represented by this group.
    pub run_id: Option<u64>,
    pub category: WorkflowCategory,
    /// Name of the linked CodePipeline (set by orchestrator from LinkMap).
    pub linked_pipeline: Option<String>,
}
```

- [ ] **Step 2: Add `linked_pipeline: None` to every `WorkflowGroup` literal**

Every `WorkflowGroup { ... }` in the codebase needs `linked_pipeline: None`. The
affected files and approximate counts:

- `src/model.rs` — 4 test constructors (lines ~446, 462, 475, 490)
- `src/app.rs` — 3 test constructors (lines ~148, 164, 196)
- `src/linkage.rs` — ~15 test constructors (lines ~434, 477, 510, 520, 555, 599, 632,
  715, 751, 782, 821, 855, 887, 917)
- `src/poller/mod.rs` — 1 production + 2 test (lines ~363, 1064, 1072)
- `src/ui/bar.rs` — 1 test helper `make_group` (line ~339)
- `src/ui/mod.rs` — ~20 test constructors (lines ~697, 705, 731, 739, 791, 799, 814,
  828, 839, 854, 873, 887, 895, 903, 992, 1000, 1017, 1025, 1033, 1041, 1061, 1069,
  1119, 1202)

Add `linked_pipeline: None,` after the `category` field in each.

- [ ] **Step 3: Run tests to verify nothing broke**

Run: `cargo test`
Expected: all existing tests pass (no behavior change, just a new field defaulting
to `None`).

- [ ] **Step 4: Run clippy and fmt**

Run: `cargo clippy && cargo fmt`
Expected: clean

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "model: add linked_pipeline to WorkflowGroup"
```

---

### Task 2: Cache structs and LinkMap serialization methods

**Files:**
- Modify: `src/linkage.rs` (add structs + methods after existing `LinkMap` impl)
- Test: `src/linkage.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write failing tests for `to_cache` and `load_from_cache`**

Add to the `#[cfg(test)] mod tests` in `src/linkage.rs`:

```rust
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
        map.add_discovered(
            "pipe-a".into(),
            "WF-A".into(),
            "b".into(),
            "k".into(),
        );
        let cache = map.to_cache("2026-03-27T00:00:00Z");

        let mut map2 = LinkMap::new();
        map2.load_from_cache(cache);
        assert_eq!(map2.workflow_for_pipeline("pipe-a"), Some("WF-A"));
        assert_eq!(map2.links().len(), 1);
    }

    #[test]
    fn load_from_cache_replaces_existing() {
        let mut map = LinkMap::new();
        map.add_discovered(
            "old".into(),
            "OLD-WF".into(),
            "b".into(),
            "k".into(),
        );

        let mut fresh = LinkMap::new();
        fresh.add_discovered(
            "new".into(),
            "NEW-WF".into(),
            "b2".into(),
            "k2".into(),
        );
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib linkage::tests::to_cache_empty`
Expected: FAIL — `to_cache` method doesn't exist yet.

- [ ] **Step 3: Add cache structs and LinkMap methods**

Add these **before** the `impl LinkMap` block in `src/linkage.rs` (after the
`use` statements at top):

```rust
use serde::{Deserialize, Serialize};

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
```

Add these methods **inside** the existing `impl LinkMap` block:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib linkage::tests`
Expected: all pass including the 5 new cache tests.

- [ ] **Step 5: Commit**

```bash
git add src/linkage.rs && git commit -m "linkage: add cache structs and LinkMap serialization"
```

---

### Task 3: Cache file load/save functions

**Files:**
- Modify: `src/linkage.rs` (add standalone functions)
- Test: `src/linkage.rs` (inline tests)

- [ ] **Step 1: Write failing tests for `load_link_cache` and `save_link_cache`**

Add to `#[cfg(test)] mod tests` in `src/linkage.rs`. The `tempfile` crate is already
a dev-dependency.

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib linkage::tests::save_and_load`
Expected: FAIL — `save_link_cache` doesn't exist yet.

- [ ] **Step 3: Add `load_link_cache` and `save_link_cache` functions**

Add as standalone functions in `src/linkage.rs` (after the `impl LinkMap` block,
before `/// Match S3 keys`):

```rust
use anyhow::Context;

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
    std::fs::write(&tmp, content.as_bytes())
        .context("failed to write .cibars-links.toml.tmp")?;
    std::fs::rename(&tmp, path).context("failed to rename link cache into place")?;
    Ok(())
}
```

Note: `anyhow` and `toml` are already in `Cargo.toml`. The `use anyhow::Context`
may need to be added to the top of the file alongside existing imports.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib linkage::tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/linkage.rs && git commit -m "linkage: add load/save link cache functions"
```

---

### Task 4: App fields and health check

**Files:**
- Modify: `src/app.rs:16-67` (struct + `new()`)
- Test: `src/app.rs` (inline tests)

- [ ] **Step 1: Write failing tests for `check_linkage_health`**

Add to `#[cfg(test)] mod tests` in `src/app.rs`:

```rust
    use crate::linkage::LinkMap;

    #[test]
    fn check_linkage_health_skips_while_loading() {
        let mut app = App::new();
        let mut link_map = LinkMap::new();
        link_map.add_discovered("pipe".into(), "wf".into(), "b".into(), "k".into());
        // loading_pipelines is true by default
        app.check_linkage_health(&link_map);
        assert!(!app.linkage_broken, "should not flag broken while loading");
    }

    #[test]
    fn check_linkage_health_empty_links_not_broken() {
        let mut app = App::new();
        app.loading_pipelines = false;
        app.loading_actions = false;
        let link_map = LinkMap::new();
        app.check_linkage_health(&link_map);
        assert!(!app.linkage_broken);
    }

    #[test]
    fn check_linkage_health_ghost_pipeline() {
        let mut app = App::new();
        app.loading_pipelines = false;
        app.loading_actions = false;
        // Workflow exists but no matching pipeline
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "missing-pipe".into(),
            "CI".into(),
            "b".into(),
            "k".into(),
        );
        app.check_linkage_health(&link_map);
        assert!(app.linkage_broken, "ghost pipeline should flag broken");
    }

    #[test]
    fn check_linkage_health_ghost_workflow() {
        let mut app = App::new();
        app.loading_pipelines = false;
        app.loading_actions = false;
        // Pipeline exists but no matching workflow
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        });
        let mut link_map = LinkMap::new();
        link_map.add_discovered(
            "deploy".into(),
            "missing-wf".into(),
            "b".into(),
            "k".into(),
        );
        app.check_linkage_health(&link_map);
        assert!(app.linkage_broken, "ghost workflow should flag broken");
    }

    #[test]
    fn check_linkage_health_all_present() {
        let mut app = App::new();
        app.loading_pipelines = false;
        app.loading_actions = false;
        app.workflow_groups.push(WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::default(),
            linked_pipeline: None,
        });
        app.pipeline_groups.push(PipelineGroup {
            name: "deploy".into(),
            stages: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            pending_link: false,
        });
        let mut link_map = LinkMap::new();
        link_map.add_discovered("deploy".into(), "CI".into(), "b".into(), "k".into());
        app.check_linkage_health(&link_map);
        assert!(!app.linkage_broken, "all present should not be broken");
    }

    #[test]
    fn app_starts_with_linkage_defaults() {
        let app = App::new();
        assert!(!app.linkage_broken);
        assert!(!app.linkage_discovering);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib app::tests::check_linkage`
Expected: FAIL — fields and method don't exist yet.

- [ ] **Step 3: Add fields to `App` struct and `new()`**

In `src/app.rs`, add two fields to the struct (after `boost_pressed_at`):

```rust
    /// True when cached link map has stale references.
    pub linkage_broken: bool,
    /// True while discover_links() is running.
    pub linkage_discovering: bool,
```

In `App::new()`, add after `boost_pressed_at: None,`:

```rust
            linkage_broken: false,
            linkage_discovering: false,
```

- [ ] **Step 4: Add `check_linkage_health` method**

Add inside the existing `impl App` block, after `has_any_running`:

```rust
    /// Compare link map against live data. Only meaningful after initial
    /// loading is complete (both pipelines and actions fetched at least once).
    pub fn check_linkage_health(&mut self, link_map: &crate::linkage::LinkMap) {
        if self.loading_pipelines || self.loading_actions {
            return;
        }
        if link_map.links().is_empty() {
            self.linkage_broken = false;
            return;
        }
        let ghost_pipeline = link_map
            .links()
            .iter()
            .any(|l| !self.pipeline_groups.iter().any(|pg| pg.name == l.pipeline_name));
        let ghost_workflow = link_map
            .links()
            .iter()
            .any(|l| !self.workflow_groups.iter().any(|wg| wg.name == l.workflow_name));
        self.linkage_broken = ghost_pipeline || ghost_workflow;
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib app::tests`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs && git commit -m "app: add linkage_broken/discovering fields and health check"
```

---

### Task 5: StatusBar linkage indicator

**Files:**
- Modify: `src/ui/statusbar.rs:17-24` (struct fields), `src/ui/statusbar.rs:41-112`
  (render method)
- Test: `src/ui/statusbar.rs` (inline tests)

- [ ] **Step 1: Write failing tests**

Add to `#[cfg(test)] mod tests` in `src/ui/statusbar.rs`:

```rust
    fn render_bar_with_linkage(
        linkage_broken: bool,
        linkage_discovering: bool,
    ) -> String {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: None,
            linkage_broken,
            linkage_discovering,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect()
    }

    #[test]
    fn shows_relink_hint_normally() {
        let content = render_bar_with_linkage(false, false);
        assert!(content.contains("l=relink"), "got: {content}");
        assert!(!content.contains("⚠"), "got: {content}");
    }

    #[test]
    fn shows_relink_warning_when_broken() {
        let content = render_bar_with_linkage(true, false);
        assert!(content.contains("l=relink"), "got: {content}");
        assert!(content.contains("⚠"), "got: {content}");
    }

    #[test]
    fn shows_relinking_spinner_when_discovering() {
        let content = render_bar_with_linkage(false, true);
        assert!(content.contains("relinking"), "got: {content}");
        assert!(!content.contains("l=relink"), "got: {content}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib ui::statusbar::tests::shows_relink`
Expected: FAIL — fields don't exist on `StatusBar`.

- [ ] **Step 3: Add fields to `StatusBar` struct**

In `src/ui/statusbar.rs`, add after `boost_pressed_at`:

```rust
    pub linkage_broken: bool,
    pub linkage_discovering: bool,
```

- [ ] **Step 4: Update all `StatusBar` construction sites**

There are construction sites in:

1. `src/ui/mod.rs` line ~496 (production render) — add:
   ```rust
   linkage_broken: app.linkage_broken,
   linkage_discovering: app.linkage_discovering,
   ```

2. `src/ui/statusbar.rs` test helpers `render_bar` (line ~131) and
   `render_bar_with_hook` (line ~125) and `render_bar_with_warnings` (line ~340)
   and `render_buf` (line ~495) and the `boost_flash_*` tests and
   `statusbar_renders_in_rounded_block` and `statusbar_block_border_color` — add:
   ```rust
   linkage_broken: false,
   linkage_discovering: false,
   ```

- [ ] **Step 5: Add rendering logic in the `render` method**

In `src/ui/statusbar.rs`, inside the `render` method, **after** the hook status
section (after line ~100, before the warnings section):

```rust
        // Linkage status
        if self.linkage_discovering {
            spans.push(dim_sep.clone());
            spans.push(Span::styled(
                "\u{27F3} relinking...",
                Style::default().fg(theme::POLL_SCAN),
            ));
        } else if self.linkage_broken {
            spans.push(dim_sep.clone());
            spans.push(Span::styled(
                "l=relink \u{26A0}",
                Style::default().fg(theme::STATUS_RUNNING),
            ));
        } else {
            spans.push(dim_sep.clone());
            spans.push(Span::styled(
                "l=relink",
                Style::default().fg(theme::FG_DIM),
            ));
        }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib ui::statusbar::tests`
Expected: all pass (both new and existing).

- [ ] **Step 7: Commit**

```bash
git add src/ui/statusbar.rs src/ui/mod.rs && git commit -m "statusbar: add l=relink indicator with broken/discovering states"
```

---

### Task 6: `link_notify` plumbing — UI key handler and orchestrator signal

**Files:**
- Modify: `src/main.rs` (create Notify, pass to both fns, add to orchestrator
  `select!`)
- Modify: `src/ui/mod.rs:218-226` (`run_ui` signature + key handler)

This task wires the plumbing without changing discovery logic yet.

- [ ] **Step 1: Add `link_notify` in `main()`**

In `src/main.rs`, after the `boost_notify` creation (line ~263):

```rust
    let link_notify = Arc::new(tokio::sync::Notify::new());
```

- [ ] **Step 2: Pass `link_notify` to `run_poll_orchestrator`**

Update the function signature to accept `link_notify`:

```rust
async fn run_poll_orchestrator(
    app: Arc<Mutex<App>>,
    config: Config,
    token: String,
    boost_notify: Arc<tokio::sync::Notify>,
    link_notify: Arc<tokio::sync::Notify>,
    mut sigusr1: tokio::signal::unix::Signal,
) -> Result<()> {
```

Update the spawn call in `main()` to clone and pass it:

```rust
    let poll_link = link_notify.clone();
    rt.spawn(async move {
        if let Err(e) =
            run_poll_orchestrator(poll_app, poll_config, token, poll_boost, poll_link, sigusr1)
                .await
        {
            tracing::error!("poll orchestrator failed: {e:#}");
        }
    });
```

- [ ] **Step 3: Add `link_notify` to the sleep `select!` block**

In `run_poll_orchestrator`, in the **sleep** `tokio::select!` block (line ~223),
add a new branch after the `sigusr1` branch:

```rust
            _ = link_notify.notified() => {
                tracing::info!("link re-discovery triggered by 'l' key");
                // Actual re-discovery logic will be added in Task 7
            }
```

- [ ] **Step 4: Pass `link_notify` to `run_ui`**

Update `run_ui` signature in `src/ui/mod.rs`:

```rust
pub fn run_ui(
    app: Arc<Mutex<App>>,
    mut terminal: DefaultTerminal,
    profile: &str,
    region: &str,
    repo: &str,
    boost_notify: Arc<Notify>,
    link_notify: Arc<Notify>,
    term_flag: &AtomicBool,
) -> Result<()> {
```

Update the call site in `src/main.rs`:

```rust
    let result = ui::run_ui(
        app.clone(),
        terminal,
        &config.aws_profile,
        &config.region,
        &config.github_repo,
        boost_notify,
        link_notify,
        &term_flag,
    );
```

- [ ] **Step 5: Add `'l'` key handler in `run_ui`**

In the `match key.code` block in `src/ui/mod.rs` (after the `'h'` handler,
before `_ => {}`):

```rust
                    KeyCode::Char('l') => {
                        let discovering = app
                            .lock()
                            .map(|a| a.linkage_discovering)
                            .unwrap_or(true);
                        if !discovering {
                            link_notify.notify_one();
                        }
                    }
```

- [ ] **Step 6: Run tests and clippy**

Run: `cargo test && cargo clippy`
Expected: all pass. The `link_notify` branch in the orchestrator is a no-op for now.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/ui/mod.rs && git commit -m "wire link_notify between UI and orchestrator"
```

---

### Task 7: Cache-first startup and re-discovery in orchestrator

**Files:**
- Modify: `src/main.rs` (startup logic + `run_discovery` helper + re-discovery in
  `select!` + health check + `sync_linked_pipelines`)

- [ ] **Step 1: Add `sync_linked_pipelines` helper**

Add as a standalone function in `src/main.rs` (after `discover_links`):

```rust
/// Update each WorkflowGroup's linked_pipeline from the LinkMap.
fn sync_linked_pipelines(app: &Arc<Mutex<App>>, link_map: &LinkMap) {
    let mut a = app.lock().expect("app mutex poisoned");
    for wg in &mut a.workflow_groups {
        wg.linked_pipeline = link_map
            .links()
            .iter()
            .find(|l| l.workflow_name == wg.name)
            .map(|l| l.pipeline_name.clone());
    }
}
```

- [ ] **Step 2: Add `run_discovery` helper**

Add as an async function in `src/main.rs` (after `sync_linked_pipelines`):

```rust
/// Run link discovery, persist cache, and update app state.
async fn run_discovery(
    aws: &dyn PipelineClient,
    gh: &dyn ActionsClient,
    app: &Arc<Mutex<App>>,
    cache_path: &std::path::Path,
) -> LinkMap {
    app.lock().expect("app mutex poisoned").linkage_discovering = true;
    let link_map = discover_links(aws, gh).await;

    if !link_map.links().is_empty() {
        let cache = link_map.to_cache(&chrono::Utc::now().to_rfc3339());
        if let Err(e) = linkage::save_link_cache(cache_path, &cache) {
            tracing::warn!(error = %e, "failed to save link cache");
        }
    }

    sync_linked_pipelines(app, &link_map);
    app.lock().expect("app mutex poisoned").linkage_discovering = false;
    link_map
}
```

- [ ] **Step 3: Add `cwd` parameter to `run_poll_orchestrator`**

Update the signature:

```rust
async fn run_poll_orchestrator(
    app: Arc<Mutex<App>>,
    config: Config,
    token: String,
    boost_notify: Arc<tokio::sync::Notify>,
    link_notify: Arc<tokio::sync::Notify>,
    mut sigusr1: tokio::signal::unix::Signal,
    cwd: std::path::PathBuf,
) -> Result<()> {
```

And the spawn call in `main()`:

```rust
    let poll_cwd = cwd.clone();
    rt.spawn(async move {
        if let Err(e) = run_poll_orchestrator(
            poll_app, poll_config, token, poll_boost, poll_link, sigusr1, poll_cwd,
        )
        .await
        {
            tracing::error!("poll orchestrator failed: {e:#}");
        }
    });
```

- [ ] **Step 4: Replace startup discovery with cache-first logic**

In `run_poll_orchestrator`, add `cache_path` after `link_map` initialization:

```rust
    let cache_path = cwd.join(".cibars-links.toml");
```

Replace the lazy-init block (lines ~160-166):

```rust
        // Lazy-init AWS on first need + run link discovery
        if need_aws && aws_client.is_none() {
            tracing::info!("initializing AWS client (first active poll)");
            let client = init_aws_client(&config).await;

            // Cache-first: try loading from disk first
            match linkage::load_link_cache(&cache_path) {
                Ok(Some(cache)) => {
                    link_map.load_from_cache(cache);
                    sync_linked_pipelines(&app, &link_map);
                    tracing::info!(
                        links = link_map.links().len(),
                        "loaded link cache from disk"
                    );
                }
                Ok(None) => {
                    link_map = run_discovery(&client, &gh_client, &app, &cache_path).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "corrupt link cache, re-discovering");
                    let _ = std::fs::remove_file(&cache_path);
                    link_map = run_discovery(&client, &gh_client, &app, &cache_path).await;
                }
            }

            aws_client = Some(client);
        }
```

- [ ] **Step 5: Add health check after `apply_links`**

After `linkage::apply_links(&app, &mut link_map, &mut stopped_runs);` (line ~202),
add:

```rust
        // Sync linked_pipeline on workflow groups and check health
        sync_linked_pipelines(&app, &link_map);
        {
            let mut a = app.lock().expect("app mutex poisoned");
            a.check_linkage_health(&link_map);
        }
```

- [ ] **Step 6: Implement re-discovery in `link_notify` branch**

Replace the placeholder from Task 6 in the sleep `select!`:

```rust
            _ = link_notify.notified() => {
                tracing::info!("link re-discovery triggered by 'l' key");
                link_map.clear();
                if let Some(aws) = aws_client.as_ref() {
                    link_map = run_discovery(aws, &gh_client, &app, &cache_path).await;
                }
                {
                    let mut a = app.lock().expect("app mutex poisoned");
                    a.linkage_broken = false;
                    a.check_linkage_health(&link_map);
                }
            }
```

- [ ] **Step 7: Also add `link_notify` to the mid-poll `select!` block**

In the first `tokio::select!` (the one that wraps the actual poll calls, line ~170),
add a branch so `'l'` can interrupt an in-progress poll (same as boost/SIGUSR1):

```rust
            _ = link_notify.notified() => {
                tracing::info!("link re-discovery triggered (interrupted poll)");
                link_map.clear();
                if let Some(aws) = aws_client.as_ref() {
                    link_map = run_discovery(aws, &gh_client, &app, &cache_path).await;
                }
                {
                    let mut a = app.lock().expect("app mutex poisoned");
                    a.linkage_broken = false;
                    a.check_linkage_health(&link_map);
                }
                continue;
            }
```

- [ ] **Step 8: Run tests and clippy**

Run: `cargo test && cargo clippy && cargo fmt`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src/main.rs && git commit -m "orchestrator: cache-first startup and l-key re-discovery"
```

---

### Task 8: Tree rendering — linked pipelines under workflows in Actions block

**Files:**
- Modify: `src/ui/mod.rs` (`sorted_pipeline_groups`, row counting, render loop)
- Test: `src/ui/mod.rs` (inline tests)

This is the most visually impactful change. Linked pipelines move from the
CodePipelines block into the Actions block, indented under their parent workflow.

- [ ] **Step 1: Write failing tests for `sorted_pipeline_groups` filtering**

Add to `#[cfg(test)] mod tests` in `src/ui/mod.rs`:

```rust
    #[test]
    fn sorted_pipeline_groups_filters_linked() {
        let pipelines = vec![
            PipelineGroup {
                name: "linked-pipe".into(),
                stages: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
                pending_link: false,
            },
            PipelineGroup {
                name: "unlinked-pipe".into(),
                stages: vec![],
                gone: false,
                summary_status: BuildStatus::Idle,
                pending_link: false,
            },
        ];
        let workflows = vec![WorkflowGroup {
            name: "CI".into(),
            jobs: vec![],
            gone: false,
            summary_status: BuildStatus::Idle,
            run_id: None,
            category: WorkflowCategory::CI,
            linked_pipeline: Some("linked-pipe".into()),
        }];
        let sorted = sorted_pipeline_groups(&pipelines, &workflows);
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name, "unlinked-pipe");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib ui::tests::sorted_pipeline_groups_filters`
Expected: FAIL — `sorted_pipeline_groups` doesn't accept `workflow_groups` param.

- [ ] **Step 3: Update `sorted_pipeline_groups` to filter linked pipelines**

In `src/ui/mod.rs`, change the function signature and body:

```rust
/// Sort pipeline groups: those with running stages first, then alphabetical.
/// Excludes pipelines that are linked to a workflow (they render in Actions block).
fn sorted_pipeline_groups<'a>(
    groups: &'a [PipelineGroup],
    workflow_groups: &[WorkflowGroup],
) -> Vec<&'a PipelineGroup> {
    let linked: std::collections::HashSet<&str> = workflow_groups
        .iter()
        .filter_map(|wg| wg.linked_pipeline.as_deref())
        .collect();
    let mut sorted: Vec<&PipelineGroup> = groups
        .iter()
        .filter(|g| !linked.contains(g.name.as_str()))
        .collect();
    sorted.sort_by(|a, b| {
        let a_running = a.stages.iter().any(|s| s.status == BuildStatus::Running);
        let b_running = b.stages.iter().any(|s| s.status == BuildStatus::Running);
        b_running.cmp(&a_running).then(a.name.cmp(&b.name))
    });
    sorted
}
```

- [ ] **Step 4: Update the call site in `run_ui`**

Change line ~247 from:

```rust
            let sorted_pipe_groups: Vec<&PipelineGroup> =
                sorted_pipeline_groups(&app.pipeline_groups);
```

To:

```rust
            let sorted_pipe_groups: Vec<&PipelineGroup> =
                sorted_pipeline_groups(&app.pipeline_groups, &app.workflow_groups);
```

- [ ] **Step 5: Update existing tests that call `sorted_pipeline_groups`**

Search for any existing test calls and add the `workflow_groups` parameter
(pass `&[]` for tests that don't care about linkage).

- [ ] **Step 6: Update action row counting to include linked pipeline rows**

In the `action_rows` computation (line ~254), replace the CI job count:

```rust
                let ci_jobs: usize = sorted_wf_groups
                    .iter()
                    .filter(|g| g.category == WorkflowCategory::CI)
                    .map(|g| {
                        let job_count = g.jobs.iter().filter(|j| !j.gone).count();
                        let pipeline_rows =
                            if let Some(ref pipe_name) = g.linked_pipeline {
                                if let Some(pg) = app
                                    .pipeline_groups
                                    .iter()
                                    .find(|p| p.name == *pipe_name)
                                {
                                    let visible_stages =
                                        pg.stages.iter().filter(|s| !s.gone).count();
                                    if visible_stages > 0 {
                                        1 + visible_stages // header + stages
                                    } else {
                                        1 // just header
                                    }
                                } else {
                                    0
                                }
                            } else {
                                0
                            };
                        job_count + pipeline_rows
                    })
                    .sum();
```

- [ ] **Step 7: Render linked pipeline after each CI workflow's jobs**

In the CI workflow render loop (line ~347), after the inner `for bar in
group.jobs.iter()...` loop closes, add:

```rust
                    // Render linked pipeline indented under this workflow
                    if let Some(ref pipe_name) = group.linked_pipeline {
                        if let Some(pg) = app
                            .pipeline_groups
                            .iter()
                            .find(|p| p.name == *pipe_name)
                        {
                            if row < inner_areas.len() {
                                // Pipeline header: "  └─ ● pipeline-name"
                                let dot_color = if pg.pending_link || pg.gone {
                                    theme::FG_DIM
                                } else {
                                    pg.summary_status.color()
                                };
                                let header_line = Line::from(vec![
                                    Span::styled(
                                        "  \u{2514}\u{2500} ",
                                        Style::default().fg(theme::FG_DIM),
                                    ),
                                    Span::styled(
                                        "\u{25CF} ",
                                        Style::default().fg(dot_color),
                                    ),
                                    Span::styled(
                                        &pg.name,
                                        Style::default().fg(dot_color),
                                    ),
                                ]);
                                frame.render_widget(header_line, inner_areas[row]);
                                row += 1;
                            }

                            let stage_name_width =
                                all_pipeline_stages_name_width(&app.pipeline_groups);
                            for stage in pg.stages.iter().filter(|s| !s.gone) {
                                if row >= inner_areas.len() {
                                    break;
                                }
                                let bar_dim = dim || pg.gone || pg.pending_link;
                                let stage_area = Rect::new(
                                    inner_areas[row].x + 4,
                                    inner_areas[row].y,
                                    inner_areas[row].width.saturating_sub(4),
                                    1,
                                );
                                frame.render_widget(
                                    BarWidget::new(stage, stage_name_width, bar_dim),
                                    stage_area,
                                );
                                row += 1;
                            }
                        }
                    }
```

- [ ] **Step 8: Run tests and clippy**

Run: `cargo test && cargo clippy && cargo fmt`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src/ui/mod.rs && git commit -m "ui: render linked pipelines indented under workflows"
```

---

### Task 9: Final integration test and push

**Files:** none new — validation only

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 2: Run clippy with no warnings**

Run: `cargo clippy -- -D warnings`
Expected: clean.

- [ ] **Step 3: Run fmt check**

Run: `cargo fmt -- --check`
Expected: clean.

- [ ] **Step 4: Push to master**

```bash
git push origin master
```
