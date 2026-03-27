# btop TUI reference

Reference for replicating btop's graph rendering technique and default color scheme in a custom TUI.

---

## Graph characters

btop uses **Unicode Braille Patterns** (`U+2800–U+28FF`) to render bar graphs at sub-character resolution.

### How it works

A Braille character is a 2×4 dot grid, giving 4 vertical dot positions per character cell. btop uses one column of dots per data value, so each character cell can encode **up to 4 vertical levels**. Two adjacent values share a single character (left column + right column), which doubles horizontal density compared to block characters.

Dot positions are numbered in the Braille standard as follows:

```
left  right
 1     4    ← top
 2     5
 3     6
 7     8    ← bottom
```

A bar is drawn bottom-to-top by lighting dots 7 → 3 → 2 → 1 in sequence:

| Height | Dots lit (left col) | Example char |
|--------|---------------------|--------------|
| 0%     | none                | `⠀` U+2800  |
| 25%    | 7                   | `⢀` U+28C0  |
| 50%    | 7, 3                | `⢄` U+28E4  |
| 75%    | 7, 3, 2             | `⢆` U+28F6  |
| 100%   | 7, 3, 2, 1          | `⢇` U+28F7  |

A fully filled cell (both columns, all dots): `⣿` `U+28FF`

### Graph modes

btop offers three graph symbol sets, configurable per box:

| Mode      | Characters used                     | Vertical resolution | Notes                              |
|-----------|-------------------------------------|---------------------|------------------------------------|
| `braille` | `U+2800–U+28FF`                     | 4 dots per cell     | Default. Requires font support.    |
| `block`   | `▁▂▃▄▅▆▇█` (`U+2581–U+2588`)       | 8 levels per cell   | More compatible, lower resolution. |
| `tty`     | 3 symbols only                      | 3 levels            | Works in real TTYs without UTF-8.  |

### Font requirements

The braille mode requires a font that includes Unicode Block `U+2800–U+28FF`. If characters render as all-dots-filled or misaligned, it is a font fallback issue, not a rendering bug. Fonts known to work: **DejaVu Sans**, **Terminess Powerline**, **Nerd Fonts**.

---

## Default color scheme

Source: hardcoded in `src/btop_theme.cpp`. The Default and TTY themes are built into the binary and not shipped as `.theme` files.

Colors are `#RRGGBB` hex. Graph gradients use `_start` / `_mid` / `_end` — omit `_mid` for a two-stop gradient, mapping low intensity to start and high intensity to end.

### UI chrome

| Key           | Hex       | Role                                          |
|---------------|-----------|-----------------------------------------------|
| `main_bg`     | `#000000` | Main background                               |
| `main_fg`     | `#ffffff` | Main text                                     |
| `title`       | `#ffff55` | Box title labels                              |
| `hi_fg`       | `#ff5555` | Keyboard shortcut highlights                  |
| `selected_bg` | `#555555` | Selected row background (process list)        |
| `selected_fg` | `#ffffff` | Selected row foreground                       |
| `inactive_fg` | `#555555` | Disabled / inactive text                      |
| `graph_text`  | `#bcbcbc` | Text overlaid on graphs (uptime, scale label) |
| `meter_bg`    | `#555555` | Background of percentage meters               |
| `proc_misc`   | `#00ff7f` | Mini CPU graphs, memory detail, status text   |
| `div_line`    | `#303030` | Box dividers and small box lines              |

### Box outlines

Each box has its own distinct muted border color for visual separation without relying solely on line weight.

| Key        | Hex       | Box            |
|------------|-----------|----------------|
| `cpu_box`  | `#5f8787` | CPU            |
| `mem_box`  | `#87875f` | Memory / disks |
| `net_box`  | `#5f5f87` | Network        |
| `proc_box` | `#872323` | Processes      |

### Graph gradients

#### CPU

| Key         | Hex       |
|-------------|-----------|
| `cpu_start` | `#50f095` |
| `cpu_mid`   | `#f0c050` |
| `cpu_end`   | `#f05050` |

#### Temperature

| Key          | Hex       |
|--------------|-----------|
| `temp_start` | `#4897d4` |
| `temp_mid`   | `#5474e8` |
| `temp_end`   | `#ff40b6` |

#### Process CPU

| Key             | Hex       |
|-----------------|-----------|
| `process_start` | `#50f095` |
| `process_mid`   | `#f0c050` |
| `process_end`   | `#f05050` |

### Memory meters

#### Free

| Key          | Hex       |
|--------------|-----------|
| `free_start` | `#003000` |
| `free_mid`   | `#00c000` |
| `free_end`   | `#90ff90` |

#### Cached

| Key            | Hex       |
|----------------|-----------|
| `cached_start` | `#000050` |
| `cached_mid`   | `#0090ff` |
| `cached_end`   | `#90c0ff` |

#### Available

| Key               | Hex       |
|-------------------|-----------|
| `available_start` | `#664400` |
| `available_mid`   | `#ffaa00` |
| `available_end`   | `#ffe080` |

#### Used

| Key          | Hex       |
|--------------|-----------|
| `used_start` | `#a00000` |
| `used_mid`   | `#ff4040` |
| `used_end`   | `#ff9090` |

### Network graphs

#### Download

| Key              | Hex       |
|------------------|-----------|
| `download_start` | `#000000` |
| `download_mid`   | `#0000ff` |
| `download_end`   | `#8080ff` |

#### Upload

| Key            | Hex       |
|----------------|-----------|
| `upload_start` | `#000000` |
| `upload_mid`   | `#ff00ff` |
| `upload_end`   | `#ff80ff` |

---

## Theme file format

Community themes live at `/usr/share/btop/themes/` or `~/.config/btop/themes/`. The format is:

```bash
# Single color
theme[main_bg]="#000000"

# Two-stop gradient
theme[cpu_start]="#50f095"
theme[cpu_end]="#f05050"

# Three-stop gradient
theme[cpu_start]="#50f095"
theme[cpu_mid]="#f0c050"
theme[cpu_end]="#f05050"
```

Accepted color formats: `#RRGGBB`, `#BW` (greyscale shorthand), or `R G B` as space-separated decimals (0–255).

Browsable theme archive: <https://fossies.org/linux/btop/themes/>
