# Pipeline-Centric UI Layout Design

## Context

The current UI uses source-based grouping: a "GitHub Actions" block and a "CodePipelines"
block. With linkage, linked pipelines render as tree children under GH workflows, creating
an awkward hybrid where some pipelines appear in Actions and others in CodePipelines.

The real mental model is pipeline-centric: a CI pipeline flows from GH Actions jobs through
CodePipeline stages. The UI should reflect this.

## Design

### Two Modes

**Without linkage** (no `.cibars-links.toml`): current behavior preserved — "GitHub Actions"
block + "CodePipelines" block, no changes.

**With linkage**: pipeline-centric layout as described below.

### Layout Structure (With Linkage)

```text
╭ header ───────────────────────────────────────────╮
│ profile │ region │ repo │ time                     │
╰───────────────────────────────────────────────────╯
╭ CI ───────────────────────────────────────────────╮
│● Validate Cloudformation  ████████████████  14:41 │
│● Backend Tests            ████████████████  14:42 │
│● Frontend Tests           ████████████████  14:42 │
╰───────────────────────────────────────────────────╯
╭ attracr-backend ──────────────────────────────────╮
│● Build Backend            ████████████████  14:43 │
│  └─ ● attracr-backend                            │
│    Build                  ████████████████  14:45 │
╰───────────────────────────────────────────────────╯
╭ attracr-frontend ─────────────────────────────────╮
│● Build Frontend           ████████████████  14:42 │
│  └─ ● attracr-frontend                           │
│    Source                 ████████████████  14:43 │
│    DeployInfra            ████████████████  14:43 │
│    Deploy                 ████████████████  14:45 │
╰───────────────────────────────────────────────────╯
╭ reviews ──────────────────────────────────────────╮
│● PR Checks                ████████████████  15:01 │
╰───────────────────────────────────────────────────╯
╭ statusbar ────────────────────────────────────────╯
```

### Sections (top to bottom)

1. **Header** — unchanged
2. **Shared section** — jobs needed by >1 pipeline, titled with workflow name (e.g., "CI")
3. **Pipeline blocks** — one per CodePipeline, titled with pipeline name
4. **Reviews** — review-category workflows, unchanged
5. **Statusbar** — unchanged

### Per-Job S3 Mapping (Enhanced YAML Parsing)

Currently `extract_s3_paths()` regex-scans the entire YAML. Enhance to parse per-job:

1. Traverse `yaml["jobs"]` keys
2. For each job, extract `steps[].run` fields and find S3 paths
3. Record `needs:` field per job
4. Return `Vec<JobS3Info>` with job name, S3 uploads, and dependencies

New structures:

```rust
pub struct JobS3Info {
    pub job_id: String,        // YAML job key (e.g., "build-backend")
    pub job_name: String,      // Display name from `name:` field
    pub s3_uploads: Vec<S3Upload>,
    pub needs: Vec<String>,    // Job IDs this job depends on
}

pub struct WorkflowFile {
    pub name: String,
    pub jobs: Vec<JobS3Info>,  // Per-job info (replaces flat s3_uploads)
    pub s3_uploads: Vec<S3Upload>, // Keep for backward compat
}
```

### Job Assignment Algorithm

Given a workflow's `Vec<JobS3Info>` and the pipeline link map:

1. **Direct match**: For each pipeline, find jobs whose S3 uploads match the pipeline's
   S3 source → assign to that pipeline
2. **Needs chain walk**: For each directly-matched job, walk its `needs:` recursively,
   collecting all upstream jobs → assign to same pipeline
3. **Shared detection**: Jobs assigned to >1 pipeline are "shared" → move to shared section
4. **Orphan jobs**: Jobs not reachable from any pipeline → shared section
5. **Pipeline-unique jobs**: Jobs assigned to exactly 1 pipeline → that pipeline's block

### Enhanced LinkCache

Add per-job info to the cached link:

```toml
schema_version = 2

[[links]]
pipeline_name = "attracr-backend"
workflow_name = "CI"
s3_bucket = "attracr-ci-artifacts-202508219625"
s3_key = "backend/backend-source.zip"
source = "YamlDiscovered"
job_ids = ["build-backend"]           # NEW: direct S3-uploading jobs
dep_job_ids = ["validate-cfn", "test-backend", "test-frontend"]  # NEW: needs chain
```

Bump schema_version to 2. Old v1 caches trigger re-discovery.

### Pipeline Block Title

Derived from CodePipeline name (e.g., "attracr-backend"). The shared section title is
the workflow name (e.g., "CI").

### Collapse/Expand Behavior

Each section is independently expandable/collapsible (existing `e` key behavior).
Pipeline blocks default to expanded. The shared section also defaults to expanded.

### Rendering Logic

In `ui/mod.rs`, replace the current two-section render with:

```text
if linkage exists:
  1. Compute job assignments (shared vs per-pipeline)
  2. Render shared section (workflow name title, shared jobs)
  3. For each pipeline (sorted: running first, then alphabetical):
     a. Render pipeline block (pipeline name title)
     b. Render assigned GH Actions jobs with status dots
     c. Render linked pipeline tree (└─ ● pipeline-name + stages)
  4. Render reviews section
else:
  current rendering (GitHub Actions + CodePipelines blocks)
```

### Sorting Within Pipeline Blocks

Jobs appear in dependency order (topological sort of `needs:` graph):

- Jobs with no dependencies first
- Then jobs that depend on those, etc.
- The build/upload job last (right before the pipeline stages)

### Edge Cases

- **Shared job** (needed by >1 pipeline) → shared section
- **Orphan job** (no S3, not in any needs chain) → shared section
- **Pipeline with no linked workflow** → standalone block, stages only
- **Workflow with no linked pipeline** → standalone "GitHub Actions" section
- **Runtime-correlated link** (no YAML) → all workflow jobs in pipeline block
- **Single pipeline per workflow** → no shared section; all jobs in block

### Files to Modify

- `src/poller/github.rs` — enhanced YAML parsing: per-job S3 + needs
- `src/poller/mod.rs` — new `JobS3Info` struct, update `WorkflowFile`
- `src/linkage.rs` — job assignment algorithm, updated cache schema v2
- `src/model.rs` — job-to-pipeline assignment tracking in App state
- `src/app.rs` — computed pipeline-centric layout methods
- `src/ui/mod.rs` — new rendering path for linked mode
- `src/ui/bar.rs` — title widget changes for pipeline block headers

## Verification

1. Run `cargo test` — all existing tests pass
2. Add unit tests for per-job YAML extraction with fixture YAML
3. Add unit tests for job assignment algorithm (shared detection, chain walking)
4. Add unit tests for topological sort of jobs
5. Run with attracr linkage — verify layout matches spec mockup
6. Run without linkage — verify current behavior unchanged
7. `cargo clippy` and `cargo fmt` clean
