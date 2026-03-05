# cibars

A lightweight terminal UI for monitoring CI/CD pipelines, built in Rust.

`cibars` runs as a daemon inside a tmux pane and gives you a live,
consolidated view of your AWS CodePipelines and GitHub Actions in a
single screen — no browser required.

## Features

- Live polling of AWS CodePipelines and GitHub Actions
- Intelligent polling: slow when idle, fast when builds are running
- Manual boost (`b`) or external boost (SIGUSR1) for immediate refresh
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

Each monitored pipeline or workflow gets its own labeled bar. Every
poll cycle appends one `|` to the bar while the build is running.
The bar color encodes the build state:

| Color | Meaning |
|---|---|
| Yellow | Build is running |
| Green | Build finished: succeeded |
| Red | Build finished: failed |
| Grey | No active or recent build |

When a running bar reaches the terminal edge before the build
completes, it resets to the left and starts filling again from
position 0. On completion, the entire bar switches instantly to
green or red.

```text
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
│  [e] expand  [b] boost  [q] quit                last poll: 14:32:00      │
└───────────────────────────────────────────────────────────────────────────┘
```

Yellow bar = running.  Green bar = succeeded.  Red bar = failed.

## Polling State Machine

cibars uses an intelligent polling strategy to minimize API calls
while staying responsive. AWS CodePipeline depends on GitHub
Actions, so AWS is only polled when GitHub detects running builds.

**Startup:** The first poll cycle always polls both GitHub and AWS
to give immediate visibility into current status.

```text
              boost (b key)
    ┌──────────────────────────┐
    │                          ▼
  Idle ──────────────────► Watching
  30s GH, no AWS           5s GH, no AWS
    ▲         │                │
    │         │ 5min idle      │ GH finds running
    │         ▼                ▼
  LongIdle  Cooldown ◄──── Active
  5min GH   5s GH+AWS      5s GH+AWS
  no AWS    60s timer           │
    │         │                 │ nothing running
    │         └─────────────────┘
    │  boost (b key)
    └──────────────────► Watching
```

| State    | GH interval | Poll AWS? | Entry                                               |
|----------|-------------|-----------|-----------------------------------------------------|
| Idle     | 30s         | No        | Startup (after initial poll), or cooldown expired   |
| LongIdle | 5min        | No        | 5min of Idle with no running builds                 |
| Watching | 5s          | No        | User pressed `b` from Idle or LongIdle              |
| Active   | 5s          | Yes       | GitHub detects running builds                       |
| Cooldown | 5s          | Yes       | Nothing running (from Active), 60s timer            |

**Key transitions:**

- Press `b` or send SIGUSR1 in Idle/LongIdle → Watching (fast GH-only polling)
- 5min of Idle with no running builds → LongIdle (5min polling)
- GitHub finds running builds → Active (adds AWS polling)
- All builds finish → Cooldown (keeps fast polling for 60s)
- 60s of inactivity → back to Idle
- Pressing `b` in Active/Cooldown is a no-op (already fast)

## Installation

```bash
git clone https://github.com/glnds/cibars
cd cibars
cargo build --release
cp target/release/cibars ~/.local/bin/
```

## Recommended tmux setup

```bash
tmux new-window -n cibars
tmux send-keys 'cibars --aws-profile staging --region eu-west-1 --github-repo acme/backend' Enter
```

## External boost via SIGUSR1

Send `SIGUSR1` to trigger an immediate poll boost from outside
the TUI — no pane switching needed.

Manual test:

```bash
kill -USR1 $(pgrep cibars)
```

### Git pre-push hook

Add a `pre-push` hook to the repository cibars monitors so
every `git push` auto-boosts polling:

```bash
cat > .git/hooks/pre-push << 'EOF'
#!/bin/sh
pkill -USR1 cibars 2>/dev/null
exit 0
EOF
chmod +x .git/hooks/pre-push
```

## License

MIT
