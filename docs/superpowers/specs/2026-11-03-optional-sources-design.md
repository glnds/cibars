# Optional sources (GitHub-only / CodePipeline-only)

## Problem

cibars required both an AWS profile and a GitHub repo + token. Projects that use only GitHub
Actions or only CodePipeline could not run it.

## Design

- New setting `sources` (toml array, `--sources a,b` CLI; CLI wins). Values `github`, `aws`.
  Absent = both, so existing configs keep working. Empty or unknown value = startup error.
- Validation is per enabled source: `github` needs `github_repo` (+ token, resolved only when
  enabled); `aws` needs `aws_profile` + `region`. Settings for a disabled source are ignored.
- `Config` holds `aws: Option<AwsSource>` and `github: Option<GithubSource>`.
- Runtime: `poller::poll_sources_tick` polls only enabled sources. SSO health loop runs only
  with `aws`. Linkage (cache, discovery, `l` key) runs only with both.
- UI: `App::with_sources` sets `*_enabled`; a disabled source starts not-loading so
  loading-gated logic isn't blocked. Disabled section gets height 0; header omits its fields;
  status bar drops the relink hint without linkage.
