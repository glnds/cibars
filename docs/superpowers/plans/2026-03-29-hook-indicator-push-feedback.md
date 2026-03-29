# Hook Indicator & Push Signal Feedback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a green `✓hook` indicator when the pre-push hook is installed, and flash
`⬆ pushed!` in blue for 1.5s when a SIGUSR1 push signal arrives.

**Architecture:** Add `push_signal_at: Option<Instant>` to `App` state. The SIGUSR1
branch in the poll orchestrator sets this timestamp. The statusbar renders three states:
push-signal-active → hook-installed → hook-missing. Manual `b` boost is unaffected.

**Tech Stack:** Rust, ratatui, tokio signals

**Spec:** `docs/superpowers/specs/2026-03-29-hook-indicator-push-feedback-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/app.rs` | Modify | Add `push_signal_at` field |
| `src/main.rs:206-211` | Modify | Set `push_signal_at` in SIGUSR1 branch |
| `src/ui/statusbar.rs` | Modify | Add `push_signal_at` field, render logic |
| `src/ui/mod.rs:494-503,859-868` | Modify | Pass `push_signal_at` to StatusBar |
| `tests/` | N/A | All tests inline in `statusbar.rs` |

---

### Task 1: Add `push_signal_at` to App state

**Files:**
- Modify: `src/app.rs:17-50` (App struct)
- Modify: `src/app.rs:55-77` (App::new)

- [ ] **Step 1: Write failing test**

Add to `src/app.rs` (at the bottom, in a `#[cfg(test)]` module if one exists, or
create one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_app_has_no_push_signal() {
        let app = App::new();
        assert!(app.push_signal_at.is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib app::tests::new_app_has_no_push_signal`
Expected: FAIL — `push_signal_at` field does not exist.

- [ ] **Step 3: Add field to App struct and initializer**

In `src/app.rs`, add after line 43 (`boost_pressed_at` field):

```rust
    /// When a SIGUSR1 push signal was last received, for transient UI feedback.
    pub push_signal_at: Option<Instant>,
```

In `App::new()`, add after `boost_pressed_at: None,`:

```rust
            push_signal_at: None,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib app::tests::new_app_has_no_push_signal`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "app: add push_signal_at field for SIGUSR1 feedback"
```

---

### Task 2: Set `push_signal_at` in SIGUSR1 handler

**Files:**
- Modify: `src/main.rs:206-211` (SIGUSR1 branch in tokio::select!)

- [ ] **Step 1: Write the change**

In `src/main.rs`, modify the SIGUSR1 branch (lines 206-212). After
`a.poll_state = scheduler.state();` add:

```rust
                a.push_signal_at = Some(Instant::now());
```

The full branch becomes:

```rust
            _ = sigusr1.recv() => {
                scheduler.boost();
                force_next_aws = true;
                let mut a = app.lock().expect("app mutex poisoned");
                a.poll_state = scheduler.state();
                a.push_signal_at = Some(Instant::now());
                tracing::info!(state = ?scheduler.state(), "boost triggered by SIGUSR1");
            }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`
Expected: Success (may warn about unused field — that's fine, Task 3 will use it).

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "main: set push_signal_at on SIGUSR1 receive"
```

---

### Task 3: Add `push_signal_at` to StatusBar and render hook indicator

**Files:**
- Modify: `src/ui/statusbar.rs:13` (add const)
- Modify: `src/ui/statusbar.rs:17-26` (StatusBar struct)
- Modify: `src/ui/statusbar.rs:93-102` (hook render logic)

- [ ] **Step 1: Write failing test — hook installed shows ✓hook**

Add to `src/ui/statusbar.rs` tests (after the existing `no_hook_hint_when_installed`
test around line 313):

```rust
    #[test]
    fn installed_hook_shows_checkmark() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let content: String = (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect();
        assert!(content.contains("✓hook"), "got: {content}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib ui::statusbar::tests::installed_hook_shows_checkmark`
Expected: FAIL — `push_signal_at` field does not exist on `StatusBar`.

- [ ] **Step 3: Add const and field to StatusBar**

In `src/ui/statusbar.rs`, after line 13 (`BOOST_FLASH_DURATION`), add:

```rust
const PUSH_SIGNAL_DURATION: Duration = Duration::from_millis(1500);
```

In the `StatusBar` struct (lines 17-26), add after `boost_pressed_at`:

```rust
    pub push_signal_at: Option<Instant>,
```

- [ ] **Step 4: Fix all existing StatusBar constructions in test helpers**

Every test helper that constructs a `StatusBar` needs the new field. Add
`push_signal_at: None,` after `boost_pressed_at` in these functions:

1. `render_bar_with_hook` (around line 151)
2. `render_bar_with_warnings` (around line 362)
3. `boost_flash_active` test (around line 434)
4. `boost_flash_expired` test (around line 469)
5. `boost_flash_none` test (around line 499)
6. `render_bar_with_linkage` (around line 528)
7. `statusbar_renders_in_rounded_block` test (around line 409)
8. `render_buf` (around line 568)
9. `statusbar_block_border_color` test (around line 636)

- [ ] **Step 5: Replace the hook render logic**

In `src/ui/statusbar.rs`, replace lines 93-102 (the `match self.hook_status` block)
with:

```rust
        let push_active = self
            .push_signal_at
            .is_some_and(|t| t.elapsed() < PUSH_SIGNAL_DURATION);

        if push_active {
            spans.push(dim_sep.clone());
            spans.push(Span::styled(
                "\u{2B06} pushed!",
                Style::default().fg(theme::POLL_SCAN),
            ));
        } else {
            match self.hook_status {
                HookStatus::Installed => {
                    spans.push(dim_sep.clone());
                    spans.push(Span::styled(
                        "\u{2713}hook",
                        Style::default().fg(theme::STATUS_SUCCESS),
                    ));
                }
                HookStatus::Missing | HookStatus::Incomplete => {
                    spans.push(dim_sep.clone());
                    spans.push(Span::styled(
                        "h=install pre-push hook",
                        Style::default().fg(theme::STATUS_RUNNING),
                    ));
                }
                HookStatus::NoGitDir => {}
            }
        }
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --lib ui::statusbar::tests::installed_hook_shows_checkmark`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/ui/statusbar.rs
git commit -m "statusbar: render ✓hook indicator and ⬆ pushed! flash"
```

---

### Task 4: Write tests for push signal flash and color

**Files:**
- Modify: `src/ui/statusbar.rs` (test module)

- [ ] **Step 1: Write test — push signal shows ⬆ pushed!**

```rust
    #[test]
    fn push_signal_shows_pushed_label() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: None,
            push_signal_at: Some(Instant::now()),
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let content: String = (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect();
        assert!(content.contains("pushed!"), "got: {content}");
        assert!(!content.contains("✓hook"), "got: {content}");
    }
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --lib ui::statusbar::tests::push_signal_shows_pushed_label`
Expected: PASS (already implemented in Task 3).

- [ ] **Step 3: Write test — expired push signal reverts to ✓hook**

```rust
    #[test]
    fn expired_push_signal_shows_checkmark() {
        let expired = Instant::now() - Duration::from_secs(5);
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: None,
            push_signal_at: Some(expired),
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let content: String = (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect();
        assert!(content.contains("✓hook"), "got: {content}");
        assert!(!content.contains("pushed!"), "got: {content}");
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib ui::statusbar::tests::expired_push_signal_shows_checkmark`
Expected: PASS

- [ ] **Step 5: Write test — push signal uses POLL_SCAN color**

```rust
    #[test]
    fn push_signal_uses_scan_color() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: None,
            push_signal_at: Some(Instant::now()),
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        // Find the ⬆ character
        let col = (0u16..120)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "\u{2B06}")
            .expect("⬆ not found");
        assert_eq!(buf.cell((col, 1)).unwrap().fg, theme::POLL_SCAN);
    }
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --lib ui::statusbar::tests::push_signal_uses_scan_color`
Expected: PASS

- [ ] **Step 7: Write test — ✓hook uses STATUS_SUCCESS color**

```rust
    #[test]
    fn installed_hook_uses_success_color() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::Installed,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        // Find the ✓ character
        let col = (0u16..120)
            .find(|&x| buf.cell((x, 1)).unwrap().symbol() == "\u{2713}")
            .expect("✓ not found");
        assert_eq!(buf.cell((col, 1)).unwrap().fg, theme::STATUS_SUCCESS);
    }
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test --lib ui::statusbar::tests::installed_hook_uses_success_color`
Expected: PASS

- [ ] **Step 9: Write test — NoGitDir omits hook section entirely**

```rust
    #[test]
    fn no_git_dir_omits_hook_indicator() {
        let bar = StatusBar {
            poll_state: &PollState::Idle,
            elapsed_since_poll: Duration::ZERO,
            cooldown_remaining: None,
            warnings: &[],
            hook_status: &HookStatus::NoGitDir,
            boost_pressed_at: None,
            push_signal_at: None,
            linkage_broken: false,
            linkage_discovering: false,
        };
        let area = Rect::new(0, 0, 120, 3);
        let mut buf = Buffer::empty(area);
        bar.render(area, &mut buf);
        let content: String = (0..120)
            .map(|x| buf.cell((x, 1)).unwrap().symbol().to_string())
            .collect();
        assert!(!content.contains("✓hook"), "got: {content}");
        assert!(!content.contains("h=install"), "got: {content}");
        assert!(!content.contains("pushed!"), "got: {content}");
    }
```

- [ ] **Step 10: Run test to verify it passes**

Run: `cargo test --lib ui::statusbar::tests::no_git_dir_omits_hook_indicator`
Expected: PASS

- [ ] **Step 11: Commit**

```bash
git add src/ui/statusbar.rs
git commit -m "statusbar: add tests for hook indicator and push signal"
```

---

### Task 5: Wire `push_signal_at` through UI

**Files:**
- Modify: `src/ui/mod.rs:494-503` (first StatusBar construction)
- Modify: `src/ui/mod.rs:859-868` (second StatusBar construction)

- [ ] **Step 1: Add `push_signal_at` to both StatusBar constructions**

In `src/ui/mod.rs`, find the first `StatusBar {` block (around line 494). Add after
`linkage_discovering: app.linkage_discovering,`:

```rust
            push_signal_at: app.push_signal_at,
```

Find the second `StatusBar {` block (around line 859). Add the same line after
`linkage_discovering: app.linkage_discovering,`:

```rust
                    push_signal_at: app.push_signal_at,
```

- [ ] **Step 2: Verify everything compiles**

Run: `cargo build`
Expected: Success with no warnings.

- [ ] **Step 3: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 4: Run clippy and fmt**

Run: `cargo clippy && cargo fmt --check`
Expected: No warnings, no formatting issues.

- [ ] **Step 5: Commit**

```bash
git add src/ui/mod.rs
git commit -m "ui: wire push_signal_at from App to StatusBar"
```

---

### Task 6: Final verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass (existing + new).

- [ ] **Step 2: Run clippy**

Run: `cargo clippy`
Expected: No warnings.

- [ ] **Step 3: Run fmt**

Run: `cargo fmt --check`
Expected: Clean.

- [ ] **Step 4: Push to master**

```bash
git push
```
