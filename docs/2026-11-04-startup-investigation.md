# Executive Summary

The linkage cache loads quickly but does not bootstrap visible workflow structure.
It restores associations and job names; GitHub run history must still load before workflow and job
bars appear. Cached job names also reserve UI rows that render nothing until live bars exist.
The user's proposal is sound: persist structure, render it immediately, then refresh execution
status independently. See [cache schema](../src/linkage.rs#L35),
[cache loading](../src/main.rs#L89), and [job rendering](../src/ui/mod.rs#L475).

The attracr log records an 86-second first polling cycle despite prompt
cache loading. Later cycles mostly take 1.5–2 seconds; the twentieth takes about 70 seconds.
This strongly matches startup and periodic history scans. The log establishes cycle duration,
not individual request costs; the installed binary revision was not verified. Sanitized events from
`~/.cibars/cibars.log`, November 4, 2026, UTC: AWS initialization starts 07:59:24.136973; cache loaded
07:59:24.161520; first cycle completes 08:00:50.014885; nineteenth completes 08:01:42.948937;
twentieth completes 08:02:53.893338. The nineteenth leaves 1.101 seconds before the next cycle.

## What linkage actually saves

The cache contains pipeline/workflow associations, S3 evidence, job dependencies and optional
job-name assignments. It lacks complete workflow inventory, AWS stages, execution observations
and a source identity key. Cache loading validates schema; discovery time does not expire it.
The separate GitHub run cache lives only in memory and starts empty on every launch.
See [cache fields](../src/linkage.rs#L14), [validation](../src/linkage.rs#L262),
[assignments](../src/linkage.rs#L594), and [run-cache initialization](../src/poller/github.rs#L75).

`App::new` starts with empty groups. Loading links sets assignments and updates associations on
existing groups; it creates no workflow groups or bars. Discovery also retains only workflows
containing S3 uploads and runs only when both sources are enabled. Consequently, extending linkage
alone cannot cover every workflow or GitHub-only configurations. See [empty app](../src/app.rs#L130),
[association update](../src/linkage.rs#L397), [S3 filtering](../src/poller/github.rs#L349),
and [source gate](../src/main.rs#L72).

## Why startup stays empty

Startup explicitly boosts polling. There is no requirement to trigger a build, and no commit-SHA
filter: eligible historical completed runs qualify. Filters exclude bot actors, skipped runs and,
when configured, other branches. See [initial boost](../src/main.rs#L80) and
[run selection](../src/poller/github.rs#L525).

The initial scan fetches 30 runs per page sequentially, up to 60 pages. It stops early when eligible
runs cover the workflow inventory. Never-run, bot-only, skipped-only or off-branch workflows can
prevent that coverage, forcing history exhaustion or the 1,800-run cap. The same full scan recurs
every twentieth call. Workflow summaries publish only after the scan finishes; a late-page error
discards that call's accumulated progress. See [scan policy](../src/poller/github.rs#L27),
[inventory](../src/poller/github.rs#L79), [scan loop](../src/poller/github.rs#L146),
and [publication](../src/poller/mod.rs#L256).

Job requests then run concurrently, but publication waits for all of them. One slow workflow delays
every job bar. Meanwhile, cached assignments select pipeline-centric rendering and allocate rows
from known job names. Each row draws only if its live bar exists; missing bars have no placeholder.
The fallback layout's loading message is bypassed. This produces a bordered, empty CI area despite
already knowing its labels. See [job publication](../src/poller/mod.rs#L294),
[cached rows](../src/ui/mod.rs#L305), [rendering](../src/ui/mod.rs#L475), and
[layout selection](../src/ui/mod.rs#L602).

AWS initialization and cache loading or discovery precede initial GitHub polling. In the observed
warm-cache session, initialization-start to cache-loaded takes roughly 25 milliseconds. Afterwards
the sources execute concurrently and publish independently; the outer join gates subsequent cycles,
not GitHub summary publication. See [startup ordering](../src/main.rs#L85) and
[concurrent polling](../src/poller/mod.rs#L332).

A separate failure path can make a push appear necessary: the 90-second Grace window starts before
initialization. A slow, empty or failed first cycle can exhaust it and enter Sleep, which awaits a
signal rather than retrying on a timer. This was not observed in the inspected session, whose first
cycle reached Active. See [Grace duration](../src/poll_scheduler.rs#L3),
[transitions](../src/poll_scheduler.rs#L84), and [Sleep behavior](../src/main.rs#L192).

## Recommended behavior

The smallest correction is rendering cached job names as neutral placeholders immediately,
even when no live bars exist. Labels should say “loading” or “unknown”; cached structure must not
imply a successful, running or failed current execution. This directly addresses the blank area
without waiting for a larger cache redesign.

A complete solution needs a persistent topology snapshot: workflow identities and names,
known job structure, pipeline stages and linkage. Scope it by repository, branch/filter settings,
AWS account and region, and schema. On warm startup, hydrate rows before network initialization.
On first use, publish workflow inventory as soon as available and add discovered detail
progressively. GitHub exposes [workflow inventory][github-workflows] independently of runs;
AWS exposes [pipeline structure][aws-pipeline] and [pipeline version][aws-state]. Using these for
background reconciliation and version-based refresh is a design inference, not existing behavior.

Remove history completion from the first useful render. Publish recent-page results promptly, then
resolve missing known workflows through bounded, targeted queries. Retain sparse workflows when
absent from recent pages, and refresh known active executions. Publish each job result as it
finishes. Explicit request deadlines and independent source initialization would contain delays.
Simply reducing the scan limit would risk reintroducing missing-workflow bugs.

Treat structure as reusable but revisable. Refresh it in the background and on relink; distinguish
successful empty discovery from failed or partial discovery. Current discovery saves only nonempty
links, leaving stale assignments/cache after successful removal of all links. Resolve that before
using cached topology as the startup foundation. See [discovery persistence](../src/linkage.rs#L418).

Historical execution results should retain run/commit identity and freshness. Displaying a last
known result is useful if clearly identified; it must not suggest that the current commit passed.
The current code has no SHA filter, so this is a proposed presentation guarantee.

## Acceptance criteria

Regression tests should prove that cached names render before any network response; a delayed
second history page cannot hide first-page results; and one stalled job request cannot hide
completed peers. Cover first use without cache, warm startup offline, GitHub-only mode, slow AWS
initialization, never-run workflows, sparse workflows, stale active runs, and historical status
labels. Successful empty discovery must clear obsolete structure; failed refresh must preserve
known structure with an unavailable-status indication.

Existing tests establish initial boost, sparse-workflow discovery and scan bounds, but do not prove
timely partial publication or visible cached job rows. Preserve their correctness guarantees while
adding these behavioral tests. See [boost test](../src/main.rs#L825),
[sparse-workflow tests](../src/poller/github.rs#L658), and [scan-cap test](../src/poller/github.rs#L726).

## Assumptions

This is investigation and recommendation; implementation is outside this phase. The attracr log is
treated as relevant because it names that configuration, although the user did not identify the
monitored repository. Runtime/source attribution remains an inference until the installed revision
is checked. Topology usually changes less frequently than execution state, but is not immutable.
No specific startup latency target is promised without measurement after implementation.

## Validation

Formatting and clippy passed through `mise run check`. Its sandboxed test run passed 647 tests;
10 HTTP fixture tests could not bind localhost sockets. Re-running `mise exec -- cargo test --locked`
outside the sandbox passed all 657 tests. No implementation, build triggers, infrastructure changes
or application launches were performed.

[github-workflows]: https://docs.github.com/en/rest/actions/workflows#list-repository-workflows
[aws-pipeline]: https://docs.aws.amazon.com/codepipeline/latest/APIReference/API_GetPipeline.html
[aws-state]: https://docs.aws.amazon.com/codepipeline/latest/APIReference/API_GetPipelineState.html
