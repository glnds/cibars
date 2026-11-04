//! Startup "equalizer" intro: each bar plays a spectrum analyzer for a few
//! seconds the moment it first appears (no waiting for all sources). Each bar is a frequency band (top =
//! bass, bottom = treble) driven by a 70 BPM groove and a moving melodic spectrum.
//! Kick, backbeat snare and off-beat hats share one clock. Rounded attacks, decay,
//! and a peak block that holds then falls. Render-only: never touches Bar state.

use std::time::Duration;

use ratatui::style::Color;

use super::theme;

/// How long the intro plays.
pub const INTRO_DURATION: Duration = Duration::from_secs(3);
/// Frame interval while the intro plays (~30 fps).
pub const INTRO_FRAME: Duration = Duration::from_millis(33);
/// VU low end: btop `cpu_start` green.
pub const INTRO_LOW: Color = Color::Rgb(80, 240, 149);

/// Seconds per beat at 70 BPM (slow, relaxed groove).
pub const BEAT: f32 = 60.0 / 70.0;
/// How long a peak block sticks at its max before falling (s).
pub const PEAK_HOLD: f32 = 0.22;
/// Peak fall speed after the hold (full height per second).
const PEAK_FALL: f32 = 1.0;
/// Peak-hold sampling grid (s).
const PEAK_STEP: f32 = 0.025;
/// Fade-out at the end of the intro (s), so real bars take over smoothly.
const FADE_OUT: f32 = 0.4;
const FADE_IN: f32 = 0.16;
const ATTACK: f32 = 0.035;

const KICK_DECAY: f32 = 0.25;
const SNARE_DECAY: f32 = 0.3;
const HAT_DECAY: f32 = 0.15;

/// One rendered intro frame for a bar: current level and held peak, in [0, 1].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntroFrame {
    pub level: f32,
    pub peak: f32,
}

/// Raw analyzer level at `t` seconds for a band (0 = bass, 1 = treble).
pub fn band_level(t: f32, band: f32, seed: u64) -> f32 {
    let t = t.max(0.0);
    let band = band.clamp(0.0, 1.0);
    let beat = t / BEAT;
    let beat_idx = beat.floor() as u64;
    let mut drums = 0.0;
    // Retain earlier tails across beat boundaries instead of resetting the envelope.
    for index in beat_idx.saturating_sub(4)..=beat_idx {
        let age = t - index as f32 * BEAT;
        let accent = [1.0, 0.86, 0.94, 0.82][index as usize % 4];
        drums += envelope(age, KICK_DECAY) * (1.0 - band).powi(3) * accent * 0.95;
        if index % 2 == 1 {
            drums += envelope(age, SNARE_DECAY) * spectral_band(band, 0.5, 0.23) * 0.72;
        }
        drums += envelope(age - BEAT / 2.0, HAT_DECAY)
            * band.powi(2)
            * (0.60 + 0.10 * jitter(17, index));
    }

    // Assumption: a silent simulated track. Shared melodic motion gives adjacent
    // frequency bands a coherent shape; a small per-bar gain avoids identical rows.
    let phrase = beat * std::f32::consts::TAU / 8.0;
    let melody_center = 0.36 + 0.13 * phrase.sin();
    let melody =
        spectral_band(band, melody_center, 0.26) * (0.26 + 0.12 * (phrase * 2.0 + 0.7).sin());
    let shimmer = spectral_band(band, 0.72, 0.25)
        * (0.08 + 0.06 * (beat * std::f32::consts::TAU + band * 4.0).sin());
    let gain = 0.94 + 0.12 * jitter(seed, 0);
    ((0.08 + melody + shimmer + drums) * gain).clamp(0.0, 1.0)
}

fn smoothstep(value: f32) -> f32 {
    let x = value.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn envelope(age: f32, decay: f32) -> f32 {
    smoothstep(age / ATTACK) * (-(age - ATTACK).max(0.0) / decay).exp()
}

fn spectral_band(band: f32, center: f32, width: f32) -> f32 {
    (-((band - center) / width).powi(2)).exp()
}

/// Deterministic pseudo-random value in [0, 1) per (seed, step).
fn jitter(seed: u64, step: u64) -> f32 {
    let h = (seed ^ step.wrapping_mul(0x9E37_79B9_7F4A_7C15)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    (h >> 40) as f32 / (1u64 << 24) as f32
}

/// Peak-hold meter over `level`: max recently reached, held for
/// [`PEAK_HOLD`], then falling at [`PEAK_FALL`].
pub fn peak_hold(t: f32, level: impl Fn(f32) -> f32) -> f32 {
    let window = PEAK_HOLD + 1.0 / PEAK_FALL;
    let first = ((t - window).max(0.0) / PEAK_STEP).ceil() as u64;
    let last = (t / PEAK_STEP).floor() as u64;
    let held = |s: f32| level(s) - PEAK_FALL * (t - s - PEAK_HOLD).max(0.0);
    (first..=last)
        .map(|i| held(i as f32 * PEAK_STEP))
        .fold(held(t), f32::max)
        .clamp(0.0, 1.0)
}

/// Frame for one bar: the beat runs on the shared `global` clock (all bars
/// in sync); the bar's own window `own` (time since it appeared) decides
/// when it fades out and hands over to the real bar.
pub fn bar_frame(global: Duration, own: Duration, band: f32, seed: u64) -> Option<IntroFrame> {
    if own >= INTRO_DURATION {
        return None;
    }
    let left = (INTRO_DURATION - own).as_secs_f32();
    let fade = smoothstep(left / FADE_OUT) * smoothstep(own.as_secs_f32() / FADE_IN);
    let t = global.as_secs_f32();
    Some(IntroFrame {
        level: band_level(t, band, seed) * fade,
        peak: peak_hold(t, |s| band_level(s, band, seed)) * fade,
    })
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

    const BASS: f32 = 0.0;
    const MID: f32 = 0.5;
    const TREBLE: f32 = 1.0;

    #[test]
    fn attacks_are_continuous_across_note_boundaries() {
        for subdivision in 1..24 {
            let t = subdivision as f32 * BEAT / 4.0;
            for band in [BASS, 0.25, MID, 0.75, TREBLE] {
                let jump = (band_level(t + 0.001, band, 7) - band_level(t - 0.001, band, 7)).abs();
                assert!(jump < 0.12, "band={band} t={t} jump={jump}");
            }
        }
    }

    #[test]
    fn melody_keeps_midrange_alive_between_drum_hits() {
        let levels: Vec<f32> = (0..8)
            .map(|beat| band_level((beat as f32 + 0.65) * BEAT, 0.35, 7))
            .collect();
        assert!(levels.iter().all(|level| *level > 0.2), "{levels:?}");
        let min = levels.iter().copied().fold(1.0, f32::min);
        let max = levels.iter().copied().fold(0.0, f32::max);
        assert!(max - min > 0.08, "phrase should move: {levels:?}");
    }

    #[test]
    fn newly_arriving_bar_eases_into_shared_track() {
        let start = bar_frame(at(1000), Duration::ZERO, MID, 7).unwrap();
        assert!(start.level < 0.01 && start.peak < 0.01);
        let entered = bar_frame(at(1000), at(200), MID, 7).unwrap();
        assert!(entered.level > start.level);
    }

    /// A bar present from the start: own window = global clock.
    fn intro_frame(elapsed: Duration, band: f32, seed: u64) -> Option<IntroFrame> {
        bar_frame(elapsed, elapsed, band, seed)
    }

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn none_after_intro() {
        assert!(intro_frame(INTRO_DURATION, BASS, 1).is_none());
        assert!(intro_frame(INTRO_DURATION + Duration::from_secs(1), MID, 1).is_none());
    }

    #[test]
    fn fades_out_at_end() {
        // real bars take over smoothly
        let tail = INTRO_DURATION - at(10);
        for seed in 0..20 {
            for band in [BASS, MID, TREBLE] {
                let f = intro_frame(tail, band, seed).unwrap();
                assert!(f.level < 0.05 && f.peak < 0.05, "{f:?}");
            }
        }
    }

    #[test]
    fn stays_in_unit_range_and_peak_above_level() {
        for ms in (0..INTRO_DURATION.as_millis() as u64).step_by(23) {
            for b in 0..=10 {
                let band = b as f32 / 10.0;
                let f = intro_frame(at(ms), band, 7).unwrap();
                assert!((0.0..=1.0).contains(&f.level), "ms={ms} {f:?}");
                assert!((0.0..=1.0).contains(&f.peak), "ms={ms} {f:?}");
                assert!(f.peak >= f.level - 1e-6, "peak below level ms={ms} {f:?}");
            }
        }
    }

    #[test]
    fn deterministic() {
        assert_eq!(intro_frame(at(1234), MID, 7), intro_frame(at(1234), MID, 7));
    }

    #[test]
    fn kick_drives_bass_on_every_beat() {
        for beat in 0..4 {
            let hit = beat as f32 * BEAT;
            assert!(band_level(hit + 0.04, BASS, 3) > 0.7, "beat {beat}");
            assert!(
                band_level(hit + 0.75 * BEAT, BASS, 3) < 0.35,
                "beat {beat} decays"
            );
        }
    }

    #[test]
    fn snare_hits_mids_on_two_and_four() {
        let beat1 = band_level(0.04, MID, 3);
        let beat2 = band_level(BEAT + 0.04, MID, 3);
        let beat4 = band_level(3.0 * BEAT + 0.04, MID, 3);
        assert!(beat2 > beat1 + 0.2, "b1={beat1} b2={beat2}");
        assert!(beat4 > beat1 + 0.2, "b1={beat1} b4={beat4}");
    }

    #[test]
    fn hihat_hits_treble_on_offbeats() {
        let on = band_level(0.04, TREBLE, 3);
        let off = band_level(BEAT / 2.0 + 0.04, TREBLE, 3);
        assert!(off > on + 0.3, "on={on} off={off}");
    }

    #[test]
    fn fast_attack_slow_decay() {
        let before = band_level(BEAT - 0.02, BASS, 3);
        // Rounded attack reaches its peak within 40 ms, then releases gradually.
        let hit = band_level(BEAT + 0.04, BASS, 3);
        let after = band_level(BEAT + 0.09, BASS, 3);
        assert!(hit - before > 0.5, "attack: {before} -> {hit}");
        assert!(hit - after < 0.4, "decay too fast: {hit} -> {after}");
    }

    #[test]
    fn bands_differ_like_a_spectrum() {
        // on the off-beat treble is up while bass has decayed
        let t = at((BEAT * 1000.0 * 2.5) as u64 + 40);
        let bass = intro_frame(t, BASS, 3).unwrap().level;
        let treble = intro_frame(t, TREBLE, 3).unwrap().level;
        assert!((bass - treble).abs() > 0.2, "bass={bass} treble={treble}");
    }

    #[test]
    fn peak_holds_then_falls() {
        let impulse = |t: f32| if t < 0.05 { 1.0 } else { 0.0 };
        assert!(peak_hold(0.15, impulse) > 0.95, "held");
        let falling = peak_hold(0.05 + PEAK_HOLD + 0.2, impulse);
        let expected = 1.0 - PEAK_FALL * 0.2;
        assert!(
            (falling - expected).abs() < 0.06,
            "falling: {falling} vs {expected}"
        );
        assert!(peak_hold(2.0, impulse) < 0.01, "dropped");
    }

    #[test]
    fn peak_sticks_above_decayed_bass() {
        let f = intro_frame(at(600), BASS, 3).unwrap();
        assert!(f.peak > f.level + 0.25, "peak trails kick release: {f:?}");
        assert!(f.level < 0.35, "level decayed: {f:?}");
    }

    #[test]
    fn bar_frame_ends_after_its_own_window() {
        assert!(bar_frame(at(5000), INTRO_DURATION, BASS, 1).is_none());
        assert!(bar_frame(at(5000), at(100), BASS, 1).is_some());
    }

    #[test]
    fn bar_frame_follows_global_beat() {
        // bar that joined late still hits on the shared kick
        let global = at((BEAT * 1000.0 * 3.0) as u64 + 10);
        let f = bar_frame(global, at(500), BASS, 1).unwrap();
        assert_eq!(f.level, band_level(global.as_secs_f32(), BASS, 1));
    }

    #[test]
    fn bar_frame_fades_at_end_of_own_window() {
        let f = bar_frame(at(1000), INTRO_DURATION - at(10), BASS, 1).unwrap();
        assert!(f.level < 0.05 && f.peak < 0.05, "{f:?}");
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
    fn tempo_is_70_bpm() {
        assert!((60.0 / BEAT - 70.0).abs() < 1e-3, "bpm = {}", 60.0 / BEAT);
    }

    #[test]
    fn bars_glide_down_not_snap() {
        // between two 50 ms frames no band may fall by more than a quarter
        let frame = INTRO_FRAME.as_secs_f32();
        for b in 0..=10 {
            let band = b as f32 / 10.0;
            let mut t = 0.0;
            while t < 2.5 {
                let (now, next) = (band_level(t, band, 3), band_level(t + frame, band, 3));
                assert!(now - next <= 0.25, "band {band} t={t}: {now} -> {next}");
                t += frame;
            }
        }
    }

    #[test]
    fn intro_frame_interval_faster_than_idle() {
        assert!(INTRO_FRAME < Duration::from_millis(100));
    }
}
