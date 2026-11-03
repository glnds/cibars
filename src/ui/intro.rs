//! Startup "equalizer" intro: every bar bounces like a VU meter for a few
//! seconds once the first poll lands. Render-only: never touches Bar state.

use std::f32::consts::PI;
use std::time::Duration;

use ratatui::style::Color;

use super::theme;

/// How long the intro plays.
pub const INTRO_DURATION: Duration = Duration::from_secs(3);
/// Frame interval while the intro plays (~20 fps).
pub const INTRO_FRAME: Duration = Duration::from_millis(50);
/// VU low end: btop `cpu_start` green.
pub const INTRO_LOW: Color = Color::Rgb(80, 240, 149);

/// Bar level in [0, 1] at `elapsed` into the intro, or None once it is over.
/// Deterministic per `seed`, so every bar dances its own pattern.
pub fn intro_level(elapsed: Duration, seed: u64) -> Option<f32> {
    if elapsed >= INTRO_DURATION {
        return None;
    }
    let t = elapsed.as_secs_f32();
    let p = t / INTRO_DURATION.as_secs_f32();
    // rise from silence, fade out so real bars take over smoothly
    let envelope = (PI * p).sin();
    let h = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let phase = ((h >> 40) as f32 / (1u64 << 24) as f32) * 2.0 * PI;
    let f1 = 1.3 + ((h >> 8) % 7) as f32 * 0.25;
    let f2 = 2.9 + ((h >> 16) % 5) as f32 * 0.4;
    let wave = 0.55
        + 0.30 * (2.0 * PI * f1 * t + phase).sin()
        + 0.15 * (2.0 * PI * f2 * t + 2.0 * phase).sin();
    Some((envelope * wave).clamp(0.0, 1.0))
}

/// Intro level for a bar, given time since the intro started (None = not started).
pub fn level_for(elapsed: Option<Duration>, name: &str) -> Option<f32> {
    elapsed.and_then(|e| intro_level(e, name_seed(name)))
}

/// True while the intro is playing.
pub fn is_active(elapsed: Option<Duration>) -> bool {
    elapsed.is_some_and(|e| e < INTRO_DURATION)
}

/// Stable seed from a bar name (FNV-1a).
pub fn name_seed(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// VU-meter color by cell position: green → yellow → orange. Never red.
pub fn vu_color(pos: usize, width: usize) -> Color {
    let t = if width <= 1 {
        0.0
    } else {
        pos as f32 / (width - 1) as f32
    };
    if t < 0.5 {
        theme::lerp_color(INTRO_LOW, theme::STATUS_RUNNING, t * 2.0)
    } else {
        theme::lerp_color(
            theme::STATUS_RUNNING,
            theme::STATUS_RUNNING_TIP,
            (t - 0.5) * 2.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme;
    use ratatui::style::Color;
    use std::time::Duration;

    #[test]
    fn level_is_none_after_intro() {
        assert!(intro_level(INTRO_DURATION, 1).is_none());
        assert!(intro_level(INTRO_DURATION + Duration::from_secs(1), 1).is_none());
    }

    #[test]
    fn level_starts_and_ends_near_zero() {
        // envelope: rise from silence, fade back out so real bars take over
        for seed in 0..20 {
            assert!(intro_level(Duration::ZERO, seed).unwrap() < 0.05);
            let tail = INTRO_DURATION - Duration::from_millis(10);
            assert!(intro_level(tail, seed).unwrap() < 0.05);
        }
    }

    #[test]
    fn level_stays_in_unit_range() {
        for ms in (0..INTRO_DURATION.as_millis() as u64).step_by(37) {
            for seed in 0..10 {
                let l = intro_level(Duration::from_millis(ms), seed).unwrap();
                assert!((0.0..=1.0).contains(&l), "ms={ms} seed={seed} l={l}");
            }
        }
    }

    #[test]
    fn level_is_deterministic() {
        let t = Duration::from_millis(1234);
        assert_eq!(intro_level(t, 7), intro_level(t, 7));
    }

    #[test]
    fn bars_move_independently() {
        let t = INTRO_DURATION / 2;
        let levels: Vec<f32> = (0..6).map(|s| intro_level(t, s).unwrap()).collect();
        let distinct = levels
            .iter()
            .filter(|&&l| (l - levels[0]).abs() > 0.05)
            .count();
        assert!(distinct >= 3, "levels too similar: {levels:?}");
    }

    #[test]
    fn level_moves_over_time() {
        let a = intro_level(INTRO_DURATION / 2, 3).unwrap();
        let b = intro_level(INTRO_DURATION / 2 + Duration::from_millis(150), 3).unwrap();
        assert!((a - b).abs() > 0.02, "a={a} b={b}");
    }

    #[test]
    fn seed_from_name_is_stable_and_spread() {
        assert_eq!(name_seed("build"), name_seed("build"));
        assert_ne!(name_seed("build"), name_seed("deploy"));
    }

    #[test]
    fn vu_gradient_green_to_orange_never_red() {
        assert_eq!(vu_color(0, 10), INTRO_LOW);
        assert_eq!(vu_color(9, 10), theme::STATUS_RUNNING_TIP);
        for pos in 0..40 {
            let Color::Rgb(r, g, _) = vu_color(pos, 40) else {
                panic!("rgb")
            };
            assert!(g >= 120, "pos {pos}: green {g} too low (reads red), r={r}");
        }
    }

    #[test]
    fn vu_color_single_cell() {
        assert_eq!(vu_color(0, 1), INTRO_LOW);
    }

    #[test]
    fn intro_frame_interval_faster_than_idle() {
        assert!(INTRO_FRAME < Duration::from_millis(100));
    }
}
