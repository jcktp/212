//! 212 branding and small ASCII illustrations.
//!
//! The logo is drawn from a 3x5 pixel font so it scales and degrades cleanly:
//! full (2x-wide blocks with a drop shadow), compact (half-blocks, 3 rows),
//! one-line, and pure ASCII.

/// 3x5 pixel glyphs.
const TWO: [&str; 5] = ["###", "..#", "###", "#..", "###"];
const ONE: [&str; 5] = [".#.", "##.", ".#.", ".#.", "###"];

fn pixels() -> Vec<Vec<bool>> {
    let glyphs = [TWO, ONE, TWO];
    (0..5)
        .map(|row| {
            let mut line = Vec::new();
            for (i, g) in glyphs.iter().enumerate() {
                if i > 0 {
                    line.push(false);
                }
                line.extend(g[row].chars().map(|c| c == '#'));
            }
            line
        })
        .collect()
}

/// A logo cell: `true` = ink, `false` = shadow.
pub type LogoLine = Vec<(String, bool)>;

/// Full startup logo: each pixel is two cells wide, with a one-cell shadow.
pub fn logo_full(unicode: bool) -> Vec<LogoLine> {
    let px = pixels();
    let h = px.len() + 1;
    let w = px[0].len() + 1;
    let at = |y: usize, x: usize| px.get(y).and_then(|r| r.get(x)).copied().unwrap_or(false);
    let (ink, shadow) = if unicode { ("██", "░░") } else { ("##", "..") };
    (0..h)
        .map(|y| {
            let mut segs: LogoLine = Vec::new();
            for x in 0..w {
                let (s, is_ink) = if at(y, x) {
                    (ink, true)
                } else if y > 0 && x > 0 && at(y - 1, x - 1) {
                    (shadow, false)
                } else {
                    ("  ", false)
                };
                match segs.last_mut() {
                    Some((text, k)) if *k == is_ink => text.push_str(s),
                    _ => segs.push((s.to_string(), is_ink)),
                }
            }
            segs
        })
        .collect()
}

/// Compact logo using half-blocks: three rows.
pub fn logo_compact(unicode: bool) -> Vec<String> {
    let px = pixels();
    if !unicode {
        return vec![" __  _  __ ".into(), "  _) | __)".into(), " /__ _|_/__".into()];
    }
    let at = |y: usize, x: usize| px.get(y).and_then(|r| r.get(x)).copied().unwrap_or(false);
    (0..3)
        .map(|r| {
            (0..px[0].len())
                .map(|x| match (at(r * 2, x), at(r * 2 + 1, x)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect::<String>()
        })
        .collect()
}

pub fn logo_width_full() -> u16 {
    ((pixels()[0].len() + 1) * 2) as u16
}

pub struct Illustration {
    pub unicode: &'static [&'static str],
    pub ascii: &'static [&'static str],
}

impl Illustration {
    pub fn lines(&self, unicode: bool) -> &'static [&'static str] {
        if unicode { self.unicode } else { self.ascii }
    }
}

pub const COMPUTER: Illustration = Illustration {
    unicode: &["╭─────────────╮", "│ ▗▄▄▄▄▄▄▄▄▄▖ │", "│ ▐ READY._ ▌ │", "│ ▝▀▀▀▀▀▀▀▀▀▘ │", "╰──┬───────┬──╯", " ▄▄█▄▄▄▄▄▄▄█▄▄ "],
    ascii: &["+-------------+", "| +---------+ |", "| | READY._ | |", "| +---------+ |", "+--+-------+--+", "  ===========  "],
};

pub const FLOPPY: Illustration = Illustration {
    unicode: &["╭───────╮", "│▕▔▔▔▔▔▏│", "│▕▁▁▁▁▁▏│", "│ ▗▄▄▄▖ │", "╰─▝▀▀▀▘─╯"],
    ascii: &["+-------+", "| [___] |", "|       |", "| [===] |", "+-------+"],
};

pub const SUCCESS: Illustration = Illustration {
    unicode: &["╭─────╮", "│  ✓  │", "╰─────╯"], ascii: &["+-----+", "| :-) |", "+-----+"]
};

pub const ERROR: Illustration =
    Illustration { unicode: &["  ▲  ", " ▲!▲ ", "▀▀▀▀▀"], ascii: &["  /\\  ", " /!!\\ ", "/____\\"] };

pub const DOCTOR: Illustration = Illustration {
    unicode: &["╭───╮", "│ ✚ │", "╰─┬─╯", " ╶┴╴ "],
    ascii: &["+---+", "| + |", "+-+-+", " -+- "],
};

pub const HELP: Illustration =
    Illustration { unicode: &["╭───╮", "│ ? │", "╰───╯"], ascii: &["+---+", "| ? |", "+---+"] };

pub const CHIP: Illustration = Illustration {
    unicode: &[" ╷╷╷╷╷ ", "╶█████╴", "╶█▒▒▒█╴", "╶█████╴", " ╵╵╵╵╵ "],
    ascii: &[" ||||| ", "-#####-", "-#:::#-", "-#####-", " ||||| "],
};

pub fn by_name(name: &str) -> Option<&'static Illustration> {
    Some(match name {
        "computer" => &COMPUTER,
        "floppy" | "download" => &FLOPPY,
        "success" => &SUCCESS,
        "error" => &ERROR,
        "doctor" => &DOCTOR,
        "help" => &HELP,
        "chip" | "model" => &CHIP,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn logos_have_consistent_widths() {
        for unicode in [true, false] {
            let full = logo_full(unicode);
            let widths: Vec<usize> = full.iter().map(|l| l.iter().map(|(s, _)| s.width()).sum()).collect();
            assert!(widths.iter().all(|w| *w == widths[0]), "{widths:?}");
            assert_eq!(widths[0] as u16, logo_width_full());
            let c = logo_compact(unicode);
            assert_eq!(c.len(), 3);
        }
        assert_eq!(logo_compact(true)[0], "▀▀█ ▄█  ▀▀█");
    }

    #[test]
    fn illustrations_are_rectangular_and_small() {
        for ill in [&COMPUTER, &FLOPPY, &SUCCESS, &ERROR, &DOCTOR, &HELP, &CHIP] {
            for lines in [ill.unicode, ill.ascii] {
                assert!(lines.len() <= 6);
                let w = lines[0].width();
                assert!(lines.iter().all(|l| l.width() == w), "{lines:?}");
                assert!(w <= 16);
            }
            assert!(ill.ascii.iter().all(|l| l.is_ascii()));
        }
    }
}
