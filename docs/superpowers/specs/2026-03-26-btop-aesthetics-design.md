# btop-Style Aesthetics for cibars

## Context

cibars currently uses a flat, text-only TUI layout — colored text spans with Unicode dots
and pipe separators, no borders or block widgets. This works but looks plain compared to
modern TUI tools like btop++. The goal is to adopt btop's visual language (rounded boxes,
per-section colors, block-character progress bars) while keeping cibars' information
density and responsiveness.

## Design Decisions

- **Approach:** ratatui `Block` widgets with `BorderType::Rounded` (not manual box drawing)
- **Color palette:** Tokyo Night (matches existing cibars colors)
- **All sections boxed:** header, actions, pipelines, status bar
- **Collapsed state:** 3-line boxes (top border + dots/count + bottom border)
- **Progress bars:** `█` (filled) / `░` (empty) replacing `|` / space
- **Tick indicators:** `▮` (filled) / `▯` (empty) replacing `=` / `-`
- **Gradients:** 2-span approximation on running bars (yellow→orange at write head)

## Color Palette

All colors are ratatui `Color::Rgb(r, g, b)` values from Tokyo Night:

| Element              | Color           | Hex       | ratatui                  |
|----------------------|-----------------|-----------|--------------------------|
| Header border        | Cyan            | `#7dcfff` | `Color::Rgb(125,207,255)`|
| Actions border       | Blue            | `#7aa2f7` | `Color::Rgb(122,162,247)`|
| Pipelines border     | Purple          | `#bb9af7` | `Color::Rgb(187,154,247)`|
| Status bar border    | Dark gray       | `#565f89` | `Color::Rgb(86,95,137)`  |
| Foreground text      | Light gray      | `#c0caf5` | `Color::Rgb(192,202,245)`|
| Dimmed/secondary     | Dark gray       | `#565f89` | `Color::Rgb(86,95,137)`  |
| Empty bar fill       | Darker gray     | `#3b4261` | `Color::Rgb(59,66,97)`   |
| Success              | Green           | `#9ece6a` | `Color::Rgb(158,206,106)`|
| Running              | Yellow          | `#e0af68` | `Color::Rgb(224,175,104)`|
| Running gradient tip | Orange          | `#ff9e64` | `Color::Rgb(255,158,100)`|
| Failed               | Red             | `#f7768e` | `Color::Rgb(247,118,142)`|
| Idle                 | Dark gray       | `#565f89` | `Color::Rgb(86,95,137)`  |
| Internal separator   | Darker gray     | `#3b4261` | `Color::Rgb(59,66,97)`   |

## New File: `src/ui/theme.rs`

Centralized color constants module. All UI files import from here instead of
scattering `Color::Rgb(...)` literals. Contains:

- `pub const BORDER_HEADER: Color`
- `pub const BORDER_ACTIONS: Color`
- `pub const BORDER_PIPELINES: Color`
- `pub const BORDER_STATUS: Color`
- `pub const FG_PRIMARY: Color`
- `pub const FG_DIM: Color`
- `pub const BAR_EMPTY: Color`
- `pub const STATUS_SUCCESS: Color`
- `pub const STATUS_RUNNING: Color`
- `pub const STATUS_RUNNING_TIP: Color`
- `pub const STATUS_FAILED: Color`
- `pub const STATUS_IDLE: Color`
- `pub const SEPARATOR: Color`
- Progress bar characters: `pub const BAR_FILLED: char = '█'`
- `pub const BAR_UNFILLED: char = '░'`
- `pub const TICK_FILLED: char = '▮'`
- `pub const TICK_EMPTY: char = '▯'`

## Layout Changes (`src/ui/mod.rs`)

### Current layout (flat)

```text
[Header]              Length(1)
[Actions title]       Length(1)
[job row] × N         Length(1) each
[Pipelines title]     Length(1)
[stage row] × N       Length(1) each
[Fill]                Fill(1)
[Status bar]          Length(1)
```

### New layout (boxed)

```text
[Header block]        Length(3)        ← top border + content + bottom border
[Actions block]       Length(2+N) or Length(3)  ← borders + content rows
[Pipelines block]     Length(2+N) or Length(3)  ← borders + content rows
[Fill]                Fill(1)
[Status block]        Length(3)        ← top border + content + bottom border
```

Each section becomes a ratatui `Block` with:

```rust
Block::bordered()
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(theme::BORDER_ACTIONS))
    .title(" GitHub Actions ")
```

Content renders into `block.inner(area)` — ratatui computes the inner rect
automatically, handling border padding.

### Height calculation

- Header block: always 3 (border + 1 content line + border)
- Status block: always 3
- Actions block expanded: 2 + action_rows (borders + content)
- Actions block collapsed: 3 (borders + dots line)
- Pipelines block expanded: 2 + pipe_rows (borders + content)
- Pipelines block collapsed: 3 (borders + dots line)

### Collapsed content

When collapsed, render inside the block:

```text
│ ●●●● (4 jobs)                                           │
```

Status dots retain their status colors. Count shows total visible items.

## Progress Bar Changes (`src/ui/bar.rs`)

### Current

```text
  ● build           [|||||       ] 2m ago
```

Uses `|` for filled and space for empty, wrapped in `[` `]`.

### New

```text
  ● build           ████████████████░░░░░░░░ 2m ago
```

- No brackets — block chars are self-delimiting
- Filled: `█` in status color
- Empty: `░` in `BAR_EMPTY` color
- Running bars: split filled portion into two spans:
  - Main fill: `█` in `STATUS_RUNNING` (yellow)
  - Write head (last 2-4 chars of fill): `█` in `STATUS_RUNNING_TIP` (orange)
- The `│` side borders are rendered by the parent `Block`, not by `BarWidget`

### Bar width calculation adjustment

Current overhead: name_col + dot(2) + brackets(2) + timestamp(7) = name + 11
New overhead: name_col + dot(2) + timestamp(7) + block_border(2) = name + 11

Net overhead stays the same (brackets removed, block borders added), so
`fill_width` calculation in the animation tick logic is unchanged.

## Header Changes (`src/ui/header.rs`)

### Current

Single line with pipe separators:

```text
cibars (v0.5.0) | profile | region | repo | 14:32:05
```

### New

Rendered inside a `Block` with cyan border and title:

```text
╭─── cibars v0.5.0 ──────────────────────────────────────╮
│ profile │ region │ repo │ 14:32:05                      │
╰─────────────────────────────────────────────────────────╯
```

- Title: `" cibars v0.5.0 "` embedded in border via `Block::title()`
- Content: profile, region, repo, timestamp separated by dim `│` chars
- SSO expired warning still turns profile segment red

## Status Bar Changes (`src/ui/statusbar.rs`)

### Current

```text
Fast Polling: [===--] | e=expand b=boost q=quit | h=install hook
```

### New

Rendered inside a `Block` with dark gray border:

```text
╭──────────────────────────────────────────────────────────╮
│ Fast ▮▮▮▯▯ │ e=expand b=boost q=quit │ h=install hook   │
╰──────────────────────────────────────────────────────────╯
```

- Tick chars: `▮` (filled, green) / `▯` (empty, dim)
- No "Polling:" label — block context makes it clear
- Separator `│` in dim color between sections

## Model Changes (`src/model.rs`)

`BuildStatus::color()` method switches from ANSI named colors to RGB:

```rust
pub fn color(&self) -> Color {
    match self {
        Self::Running => theme::STATUS_RUNNING,
        Self::Succeeded => theme::STATUS_SUCCESS,
        Self::Failed => theme::STATUS_FAILED,
        Self::Idle => theme::STATUS_IDLE,
    }
}
```

## Files Modified

| File                    | Change scope                                    |
|-------------------------|-------------------------------------------------|
| `src/ui/theme.rs`       | **New** — color constants and char constants     |
| `src/ui/mod.rs`         | Layout restructuring to nested blocks            |
| `src/ui/bar.rs`         | Progress bar chars, remove brackets, gradient    |
| `src/ui/header.rs`      | Wrap in Block, title in border                   |
| `src/ui/statusbar.rs`   | Wrap in Block, tick char swap                    |
| `src/model.rs`          | `color()` → RGB values via theme constants       |

## Verification

1. `cargo test` — all existing tests pass (bar rendering tests will need
   updated expected output for new chars)
2. `cargo clippy` — no warnings
3. `cargo fmt` — clean
4. Manual visual check — run `cargo run` with real AWS/GitHub credentials,
   verify:
   - All 4 sections have rounded borders
   - Border colors match: cyan header, blue actions, purple pipelines,
     gray status
   - Progress bars use `█`/`░` with correct status colors
   - Running bars show yellow→orange gradient at write head
   - Collapsed view shows 3-line boxes with dots
   - Expand/collapse toggle (`e`) works correctly
   - Dimming still works when poll state is not Active
   - Terminal resize doesn't break borders
   - Status bar tick uses `▮`/`▯`
