//! Startup "equalizer" intro: each bar plays a spectrum analyzer for a few
//! seconds the moment it first appears (no waiting for all sources). Each bar is a frequency band (top =
//! bass, bottom = treble) driven by one shared pulse per second. A changing spectrum
//! gives rows different heights while they rise and fall together. Rounded attacks,
//! sustained crests, slow releases, and held peaks. Render-only: never touches Bar state.

use std::time::Duration;

use ratatui::style::Color;

use super::theme;

/// How long the intro plays.
pub const INTRO_DURATION: Duration = Duration::from_secs(3);
/// Frame interval while the intro plays (~30 fps).
pub const INTRO_FRAME: Duration = Duration::from_millis(33);
/// VU low end: btop `cpu_start` green.
pub const INTRO_LOW: Color = Color::Rgb(80, 240, 149);

/// Seconds per shared pulse; frame rate only controls rendering smoothness.
pub const BEAT: f32 = 1.0;
/// How long a peak block sticks at its max before falling (s).
pub const PEAK_HOLD: f32 = 0.22;
/// Peak fall speed after the hold (full height per second).
const PEAK_FALL: f32 = 1.0;
/// Peak-hold sampling grid (s).
const PEAK_STEP: f32 = 0.025;
/// Fade-out at the end of the intro (s), so real bars take over smoothly.
const FADE_OUT: f32 = 0.4;
const FADE_IN: f32 = 0.16;
const ATTACK: f32 = 0.10;
const CREST: f32 = 0.08;
const RELEASE: f32 = 0.67;

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
    let beat = (t / BEAT).floor();
    let age = t - beat * BEAT;
    let pulse = smoothstep(age / ATTACK) * (1.0 - smoothstep((age - ATTACK - CREST) / RELEASE));

    // Assumption: one visible pulse per second, with a readable crest and release.
    // Change the spectrum only between pulses, at rest: no offbeat instruments or
    // per-band phase shifts that make the two source sections appear out of sync.
    let accent = [1.0, 0.86, 0.94, 0.82][beat as usize % 4];
    let phrase = beat * std::f32::consts::TAU / 8.0;
    let center = 0.36 + 0.20 * phrase.sin();
    let spectrum = 0.48 + 0.36 * spectral_band(band, center, 0.30);
    let gain = 0.94 + 0.12 * jitter(seed, 0);
    ((0.08 + pulse * accent * spectrum) * gain).clamp(0.0, 1.0)
}

fn smoothstep(value: f32) -> f32 {
    let x = value.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
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
    fn spectrum_varies_across_the_phrase() {
        let levels: Vec<f32> = (0..8)
            .map(|beat| band_level(beat as f32 * BEAT + 0.14, 0.35, 7))
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
    fn all_bands_peak_together_regardless_of_name() {
        for beat in 0..8 {
            let peaks: Vec<usize> = (0..=10)
                .map(|b| {
                    let band = b as f32 / 10.0;
                    let seed = name_seed(&format!("row-{b}"));
                    (0..1000)
                        .max_by(|a, b| {
                            let level =
                                |ms| band_level(beat as f32 + ms as f32 / 1000.0, band, seed);
                            level(*a).total_cmp(&level(*b))
                        })
                        .unwrap()
                })
                .collect();
            let spread = peaks.iter().max().unwrap() - peaks.iter().min().unwrap();
            assert!(spread <= 33, "beat {beat}: peak times {peaks:?}");
        }
    }

    #[test]
    fn one_pulse_per_second_without_offbeat_rebounds() {
        for beat in 0..8 {
            for band in [BASS, 0.25, MID, 0.75, TREBLE] {
                let t = beat as f32;
                let crest = band_level(t + 0.14, band, 3);
                let rest = band_level(t + 0.95, band, 3);
                assert!(
                    crest > rest + 0.3,
                    "beat {beat} band {band}: {crest} vs {rest}"
                );
                assert!(rest < 0.12, "rest before next beat: {rest}");
                for ms in (180..980).step_by(20) {
                    let now = band_level(t + ms as f32 / 1000.0, band, 3);
                    let next = band_level(t + (ms + 20) as f32 / 1000.0, band, 3);
                    assert!(
                        next <= now + 1e-5,
                        "offbeat rebound: beat {beat} band {band} ms {ms}"
                    );
                }
            }
        }
    }

    #[test]
    fn pulse_has_visible_attack_crest_and_long_release() {
        for band in [BASS, MID, TREBLE] {
            let level = |age: f32| band_level(1.0 + age, band, 3);
            assert!(
                level(0.10) > level(0.033) + 0.2,
                "attack too fast: band {band}"
            );
            assert!(
                (level(0.10) - level(0.17)).abs() < 0.03,
                "crest too short: band {band}"
            );
            assert!(
                level(0.40) > level(0.95) + 0.2,
                "release too fast: band {band}"
            );
        }
    }

    #[test]
    fn bands_differ_like_a_spectrum() {
        // A shared hit still has a spectrum, rather than identical row heights.
        let t = at(1140);
        let levels: Vec<f32> = (0..=10)
            .map(|b| intro_frame(t, b as f32 / 10.0, 3).unwrap().level)
            .collect();
        let min = levels.iter().copied().fold(1.0, f32::min);
        let max = levels.iter().copied().fold(0.0, f32::max);
        assert!(
            max - min > 0.15,
            "spectrum should vary in height: {levels:?}"
        );
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
