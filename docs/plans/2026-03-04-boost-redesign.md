# Boost Redesign: 3-Tier Polling State Machine

## Problem

The current boost (`r` key) has two issues:

1. Race condition: `watch::channel` + `has_changed()` can miss the
   signal if the poll loop isn't sleeping when the key is pressed.
2. Boost forces an AWS poll, but AWS CodePipeline depends on GitHub
   Actions — there's no point polling AWS until GitHub detects
   running builds.

## Design

### State Machine

```text
              boost (b key)
    ┌──────────────────────────┐
    │                          ▼
  Idle ──────────────────► Watching
  30s GH, no AWS           5s GH, no AWS
    ▲                          │
    │                          │ GH finds running
    │                          ▼
  Cooldown ◄──────────── Active
  5s GH+AWS               5s GH+AWS
  60s timer                    │
    │                          │ nothing running
    └──────────────────────────┘
```

### States

| State | GH interval | Poll AWS? | Entry condition |
|---|---|---|---|
| Idle | 30s | No | Startup, or cooldown expired |
| Watching | 5s | No | User pressed `b` from Idle |
| Active | 5s | Yes | GH detects running builds |
| Cooldown | 5s | Yes | Nothing running (from Active) |

### Transitions

| From | Condition | To |
|---|---|---|
| Idle | boost | Watching |
| Idle | GH finds running | Active |
| Watching | GH finds running | Active |
| Watching | 60s, no running | Idle |
| Active | nothing running | Cooldown |
| Active | boost | no-op |
| Cooldown | running again | Active |
| Cooldown | 60s expired | Idle |
| Cooldown | boost | no-op |

### Key Binding

- `r` → `b` (mnemonic: **b**oost)
- Status bar: `b=boost`

### Boost Signal

Replace `watch::channel` with `AtomicBool`:

- UI thread: `boost_flag.store(true, Relaxed)` on `b` press
- Orchestrator: `boost_flag.swap(false, Relaxed)` at top of loop
- No race — flag stays set until consumed

### Orchestrator Loop (pseudocode)

```text
loop:
  if boost_flag.swap(false):
    scheduler.boost()     // Idle → Watching

  poll_github()           // always

  if scheduler.should_poll_aws():
    poll_aws()            // Active + Cooldown only

  any_running = app.has_any_running()
  scheduler.transition(any_running)

  sleep(scheduler.interval()) // interruptible by boost
```

## Files Changed

- `src/poll_scheduler.rs` — new Watching state, `boost()` method,
  watching timeout
- `src/main.rs` — AtomicBool instead of watch channel, updated
  orchestrator logic
- `src/ui/mod.rs` — `r` → `b` key binding
- `src/ui/statusbar.rs` — `b=boost`, show Watching state
- `src/app.rs` — no changes (PollState already in App)

## Testing

### Unit Tests (poll_scheduler.rs)

- Idle + boost → Watching
- Watching + running → Active
- Watching + 60s timeout → Idle
- Active + not running → Cooldown
- Cooldown + running → Active
- Cooldown + 60s → Idle
- Boost no-op in Active/Cooldown
- `should_poll_aws()` per state
- `interval()` per state
- Watching timeout uses 60s

### Updated Tests

- statusbar: `b=boost` (was `r=boost`)
