# cibars

A lightweight terminal UI for monitoring CI/CD pipelines, built in Rust.

`cibars` runs as a daemon inside a tmux pane and gives you a live, consolidated view of your AWS CodePipelines and GitHub Actions in a single screen — no browser required.

## Features

- Live polling of all AWS CodePipelines in a given account/region (every 30 seconds)
- Live polling of all GitHub Actions runs for a given repository (every 30 seconds)
- htop-inspired terminal UI: compact, color-coded, always up-to-date
- Visual indicators for running, succeeded, and failed builds
- Auto-discovery of new pipelines and workflow runs — no restart needed
- Designed to run persistently inside a tmux window

## Requirements

- Rust 1.78+
- An AWS profile configured via `~/.aws/credentials` or `AWS_PROFILE`
- A GitHub personal access token with `repo` and `actions:read` scope, set via `GITHUB_TOKEN`

## Usage

```bash
cibars --aws-profile <profile> --region <region> --github-repo <owner/repo>
```

### Arguments

| Argument | Description | Example |
|---|---|---|
| `--aws-profile` | AWS named profile to use | `staging` |
| `--region` | AWS region | `eu-west-1` |
| `--github-repo` | GitHub repository in `owner/repo` format | `acme/backend` |

### Environment variables

| Variable | Required | Description |
|---|---|---|
| `GITHUB_TOKEN` | Yes | GitHub PAT with `repo` and `actions:read` scope |

## UI Layout

Each monitored pipeline or workflow gets its own labeled bar. Every poll cycle appends one `|` to the bar while the build is running. The bar color encodes the build state:

| Color | Meaning |
|---|---|
| Yellow | Build is running |
| Green | Build finished: succeeded |
| Red | Build finished: failed |
| Grey | No active or recent build |

When a running bar reaches the terminal edge before the build completes, it resets to the left and starts filling again from position 0. On completion, the entire bar (all filled `|` characters) switches instantly to green or red.

```
┌─ cibars ── eu-west-1 / acme/backend ──────────────────────── 14:32:05 ─┐
│                                                                           │
│  CodePipelines                                                            │
│  backend-deploy   [||||||||||||||||||||||||||||||||||||||||||||||||||||  ] │
│  frontend-deploy  [||||||||||||||||||||                                  ] │
│  infra-pipeline   [||||||||||||||||||||||||||||||||||||||||||||||||||||  ] │
│                                                                           │
│  GitHub Actions                                                           │
│  CI / test        [||||||||||||||||||||||||||||||||||||||||||||||||||||  ] │
│  Release/publish  [||||||||||||||||||||||||||||||||                      ] │
│                                                                           │
│  [r] refresh  [q] quit                          last poll: 14:32:00      │
└───────────────────────────────────────────────────────────────────────────┘
```

Yellow bar = running.  Green bar = succeeded.  Red bar = failed.

## Installation

```bash
git clone https://github.com/your-org/cibars
cd cibars
cargo build --release
cp target/release/cibars ~/.local/bin/
```

## Recommended tmux setup

```bash
tmux new-window -n cibars
tmux send-keys 'cibars --aws-profile staging --region eu-west-1 --github-repo acme/backend' Enter
```

## License

MIT
