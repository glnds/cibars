//! Startup "equalizer" intro: once the first poll lands, the bars play a
//! spectrum analyzer for a few seconds. Each bar is a frequency band (top =
//! bass, bottom = treble) driven by a 128 BPM four-on-the-floor beat: kick on
//! every beat, snare on 2 and 4, hi-hat on the off-beats. Fast attack, decay,
//! and a peak block that holds then falls. Render-only: never touches Bar state.

use std::time::Duration;

use ratatui::style::Color;

use super::theme;

/// How long the intro plays.
pub const INTRO_DURATION: Duration = Duration::from_secs(3);
/// Frame interval while the intro plays (~20 fps).
pub const INTRO_FRAME: Duration = Duration::from_millis(50);
/// VU low end: btop `cpu_start` green.
pub const INTRO_LOW: Color = Color::Rgb(80, 240, 149);

/// Seconds per beat at 128 BPM.
pub const BEAT: f32 = 60.0 / 128.0;
/// How long a peak block sticks at its max before falling (s).
pub const PEAK_HOLD: f32 = 0.5;
/// Peak fall speed after the hold (full height per second).
const PEAK_FALL: f32 = 2.0;
/// Peak-hold sampling grid (s).
const PEAK_STEP: f32 = 0.025;
/// Fade-out at the end of the intro (s), so real bars take over smoothly.
const FADE_OUT: f32 = 0.4;

const KICK_DECAY: f32 = 0.12;
const SNARE_DECAY: f32 = 0.15;
const HAT_DECAY: f32 = 0.05;

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
    let beat_idx = (t / BEAT).floor();
    let since_beat = t - beat_idx * BEAT;

    let kick = (-since_beat / KICK_DECAY).exp() * (1.0 - band).powi(2) * 0.95;

    let snare = if beat_idx as u64 % 2 == 1 {
        let mid = (1.0 - (band - 0.5).abs() * 2.5).max(0.0);
        (-since_beat / SNARE_DECAY).exp() * mid * 0.8
    } else {
        0.0
    };

    let half = BEAT / 2.0;
    let hat = if t >= half {
        let since_off = t - half - ((t - half) / BEAT).floor() * BEAT;
        (-since_off / HAT_DECAY).exp() * band.powi(2) * 0.7
    } else {
        0.0
    };

    // per-16th jitter so bands never look perfectly clean
    let step = (t / (BEAT / 4.0)).floor() as u64;
    let h = (seed ^ step.wrapping_mul(0x9E37_79B9_7F4A_7C15)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let noise = 0.05 + 0.15 * ((h >> 40) as f32 / (1u64 << 24) as f32);

    (kick + snare + hat + noise).clamp(0.0, 1.0)
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

/// Intro frame at `elapsed` for a band, or None once the intro is over.
pub fn intro_frame(elapsed: Duration, band: f32, seed: u64) -> Option<IntroFrame> {
    if elapsed >= INTRO_DURATION {
        return None;
    }
    let t = elapsed.as_secs_f32();
    let left = INTRO_DURATION.as_secs_f32() - t;
    let fade = (left / FADE_OUT).min(1.0);
    let level = band_level(t, band, seed);
    let peak = peak_hold(t, |s| band_level(s, band, seed));
    Some(IntroFrame {
        level: level * fade,
        peak: peak * fade,
    })
}

/// Intro frame for a bar given time since the intro started (None = not
/// started). `band` comes from the bar's row; falls back to the name seed.
pub fn frame_for(elapsed: Option<Duration>, band: Option<f32>, name: &str) -> Option<IntroFrame> {
    let seed = name_seed(name);
    let band = band.unwrap_or((seed % 1000) as f32 / 999.0);
    elapsed.and_then(|e| intro_frame(e, band, seed))
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

    const BASS: f32 = 0.0;
    const MID: f32 = 0.5;
    const TREBLE: f32 = 1.0;

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
            assert!(band_level(hit + 0.01, BASS, 3) > 0.7, "beat {beat}");
            assert!(band_level(hit + 0.40, BASS, 3) < 0.35, "beat {beat} decays");
        }
    }

    #[test]
    fn snare_hits_mids_on_two_and_four() {
        let beat1 = band_level(0.01, MID, 3);
        let beat2 = band_level(BEAT + 0.01, MID, 3);
        let beat4 = band_level(3.0 * BEAT + 0.01, MID, 3);
        assert!(beat2 > beat1 + 0.2, "b1={beat1} b2={beat2}");
        assert!(beat4 > beat1 + 0.2, "b1={beat1} b4={beat4}");
    }

    #[test]
    fn hihat_hits_treble_on_offbeats() {
        let on = band_level(0.01, TREBLE, 3);
        let off = band_level(BEAT / 2.0 + 0.01, TREBLE, 3);
        assert!(off > on + 0.3, "on={on} off={off}");
    }

    #[test]
    fn fast_attack_slow_decay() {
        let before = band_level(BEAT - 0.02, BASS, 3);
        let hit = band_level(BEAT + 0.005, BASS, 3);
        let after = band_level(BEAT + 0.055, BASS, 3);
        assert!(hit - before > 0.5, "attack: {before} -> {hit}");
        assert!(hit - after < 0.4, "decay too fast: {hit} -> {after}");
    }

    #[test]
    fn bands_differ_like_a_spectrum() {
        // on the off-beat treble is up while bass has decayed
        let t = at((BEAT * 1000.0 * 2.5) as u64 + 10);
        let bass = intro_frame(t, BASS, 3).unwrap().level;
        let treble = intro_frame(t, TREBLE, 3).unwrap().level;
        assert!((bass - treble).abs() > 0.2, "bass={bass} treble={treble}");
    }

    #[test]
    fn peak_holds_then_falls() {
        let impulse = |t: f32| if t < 0.05 { 1.0 } else { 0.0 };
        assert!(peak_hold(0.3, impulse) > 0.95, "held");
        let falling = peak_hold(0.05 + PEAK_HOLD + 0.2, impulse);
        assert!((0.5..0.65).contains(&falling), "falling: {falling}");
        assert!(peak_hold(2.0, impulse) < 0.01, "dropped");
    }

    #[test]
    fn peak_sticks_above_decayed_bass() {
        let f = intro_frame(at(400), BASS, 3).unwrap();
        assert!(f.peak > 0.85, "peak held from kick: {f:?}");
        assert!(f.level < 0.35, "level decayed: {f:?}");
    }

    #[test]
    fn frame_for_uses_given_band_or_name_seed() {
        let t = Some(at(500));
        assert_eq!(
            frame_for(t, Some(TREBLE), "build"),
            intro_frame(at(500), TREBLE, name_seed("build"))
        );
        assert!(frame_for(t, None, "build").is_some());
        assert!(frame_for(None, Some(BASS), "build").is_none());
    }

    #[test]
    fn is_active_only_while_playing() {
        assert!(!is_active(None));
        assert!(is_active(Some(at(10))));
        assert!(!is_active(Some(INTRO_DURATION)));
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
