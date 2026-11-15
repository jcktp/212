//! Terminal-native animations.
//!
//! Animations are pure functions of elapsed time, so rendering is cheap and the
//! event loop only needs to tick (≈12 FPS) while something is animating. With
//! reduced motion every animation renders a single static frame and the loop
//! stops ticking entirely.

use std::time::Duration;

use config::AnimationMode;

pub const FRAME: Duration = Duration::from_millis(83);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Rotating spinner.
    Spinner,
    /// A pixel bouncing inside a short track.
    Bounce,
    /// A block sweeping back and forth.
    Blocks,
    /// Indeterminate retro loading bar.
    Bar,
    /// Scanning wave.
    Scan,
    /// Pulsing status symbol (paired with a color pulse).
    Pulse,
}

#[derive(Debug, Clone, Copy)]
pub struct Animator {
    pub mode: AnimationMode,
    pub speed: f32,
    pub unicode: bool,
}

impl Animator {
    pub fn moving(&self) -> bool {
        self.mode == AnimationMode::On
    }

    fn step(&self, elapsed: Duration, period_ms: f32) -> usize {
        if !self.moving() {
            return 0;
        }
        (elapsed.as_millis() as f32 * self.speed / period_ms) as usize
    }

    /// Phase in [0, 1) for color pulses; constant when motion is reduced.
    pub fn phase(&self, elapsed: Duration, period_ms: f32) -> f32 {
        if !self.moving() {
            return 1.0;
        }
        let t = (elapsed.as_millis() as f32 * self.speed / period_ms).fract();
        // Triangle wave 0→1→0.
        1.0 - (2.0 * t - 1.0).abs()
    }

    pub fn frame(&self, style: Style, elapsed: Duration, width: usize) -> String {
        let u = self.unicode;
        match style {
            Style::Spinner => {
                const U: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                const A: [&str; 4] = ["|", "/", "-", "\\"];
                let i = self.step(elapsed, 90.0);
                if u { U[i % U.len()].into() } else { A[i % A.len()].into() }
            }
            Style::Pulse => {
                const U: [&str; 6] = ["◇", "◈", "◆", "◆", "◈", "◇"];
                const A: [&str; 4] = [".", "o", "O", "o"];
                if !self.moving() {
                    return if u { "◆" } else { "*" }.into();
                }
                let i = self.step(elapsed, 140.0);
                if u { U[i % U.len()].into() } else { A[i % A.len()].into() }
            }
            Style::Bounce => {
                let w = width.max(3);
                let (dot, track) = if u { ("■", "·") } else { ("o", ".") };
                let pos = ping_pong(self.step(elapsed, 80.0), w);
                (0..w).map(|i| if i == pos { dot } else { track }).collect()
            }
            Style::Blocks => {
                let w = width.max(4);
                let (on, mid, off) = if u { ("▰", "▰", "▱") } else { ("#", "=", "-") };
                let pos = ping_pong(self.step(elapsed, 70.0), w);
                (0..w)
                    .map(|i| {
                        let d = (i as isize - pos as isize).unsigned_abs();
                        if d == 0 {
                            on
                        } else if d == 1 {
                            mid
                        } else {
                            off
                        }
                    })
                    .collect()
            }
            Style::Bar => {
                let w = width.max(6);
                let seg = (w / 4).max(2);
                let (on, off) = if u { ("▓", "░") } else { ("#", ".") };
                let start = self.step(elapsed, 60.0) % (w + seg);
                (0..w).map(|i| if i + seg >= start && i < start { on } else { off }).collect()
            }
            Style::Scan => {
                const U: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
                const A: [&str; 4] = ["_", "-", "=", "#"];
                let set: &[&str] = if u { &U } else { &A };
                let s = self.step(elapsed, 90.0);
                (0..width.max(4))
                    .map(|i| {
                        let x = ((i + s) % (set.len() * 2 - 2)) as isize;
                        let n = set.len() as isize - 1;
                        set[(n - (x - n).abs()) as usize]
                    })
                    .collect()
            }
        }
    }
}

fn ping_pong(step: usize, width: usize) -> usize {
    let span = width.saturating_sub(1).max(1);
    let p = step % (span * 2);
    if p <= span { p } else { span * 2 - p }
}

/// Determinate progress bar with sub-cell precision (unicode).
pub fn progress_bar(fraction: f64, width: usize, unicode: bool) -> (String, String) {
    let f = fraction.clamp(0.0, 1.0);
    let cells = f * width as f64;
    let full = cells.floor() as usize;
    let mut done: String = (if unicode { "█" } else { "#" }).repeat(full);
    let mut used = full;
    if unicode && full < width {
        let partial = ((cells - full as f64) * 8.0) as usize;
        if partial > 0 {
            done.push_str(["▏", "▎", "▍", "▌", "▋", "▊", "▉"][partial.min(7) - 1]);
            used += 1;
        }
    }
    let rest = (if unicode { "░" } else { "." }).repeat(width.saturating_sub(used));
    (done, rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn anim(mode: AnimationMode) -> Animator {
        Animator { mode, speed: 1.0, unicode: true }
    }

    #[test]
    fn frames_advance_only_when_motion_is_on() {
        let on = anim(AnimationMode::On);
        let a = on.frame(Style::Spinner, Duration::from_millis(0), 1);
        let b = on.frame(Style::Spinner, Duration::from_millis(200), 1);
        assert_ne!(a, b);
        let reduced = anim(AnimationMode::Reduced);
        for style in [Style::Spinner, Style::Bounce, Style::Blocks, Style::Bar, Style::Scan, Style::Pulse] {
            let x = reduced.frame(style, Duration::from_millis(0), 8);
            let y = reduced.frame(style, Duration::from_millis(5000), 8);
            assert_eq!(x, y, "{style:?} must be static with reduced motion");
        }
    }

    #[test]
    fn frames_keep_their_width() {
        let on = anim(AnimationMode::On);
        for t in (0..3000).step_by(83) {
            for style in [Style::Bounce, Style::Blocks, Style::Bar, Style::Scan] {
                assert_eq!(on.frame(style, Duration::from_millis(t), 10).width(), 10, "{style:?}");
            }
        }
        let ascii = Animator { unicode: false, ..on };
        assert!(ascii.frame(Style::Bar, Duration::from_millis(500), 10).is_ascii());
    }

    #[test]
    fn progress_bar_is_exact_width() {
        for f in [0.0, 0.01, 0.5, 0.99, 1.0, 1.5] {
            let (a, b) = progress_bar(f, 20, true);
            assert_eq!(a.width() + b.width(), 20, "{f}");
            let (a, b) = progress_bar(f, 20, false);
            assert_eq!(a.len() + b.len(), 20);
        }
        assert_eq!(progress_bar(1.0, 4, true).0, "████");
    }

    #[test]
    fn ping_pong_bounces() {
        let v: Vec<usize> = (0..8).map(|s| ping_pong(s, 4)).collect();
        assert_eq!(v, vec![0, 1, 2, 3, 2, 1, 0, 1]);
    }
}
