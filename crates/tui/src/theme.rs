//! Semantic color themes.
//!
//! All interface colors come from a [`Theme`]; nothing else in the UI names a
//! concrete color. Themes are authored in truecolor and degraded to 256, 16 or
//! no colors depending on the terminal.

use std::collections::BTreeMap;
use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.trim().strip_prefix('#')?;
        if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
    Ansi16,
    Mono,
}

impl ColorDepth {
    pub fn detect(setting: &str) -> Self {
        match setting {
            "truecolor" => return Self::TrueColor,
            "256" => return Self::Ansi256,
            "16" => return Self::Ansi16,
            "mono" => return Self::Mono,
            _ => {}
        }
        Self::from_env(|k| std::env::var(k).ok())
    }

    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        if get("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::Mono;
        }
        let colorterm = get("COLORTERM").unwrap_or_default().to_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Self::TrueColor;
        }
        let term = get("TERM").unwrap_or_default().to_lowercase();
        let program = get("TERM_PROGRAM").unwrap_or_default();
        if get("WT_SESSION").is_some()
            || matches!(program.as_str(), "iTerm.app" | "WezTerm" | "ghostty" | "vscode")
            || term.contains("kitty")
            || term.contains("ghostty")
            || term.contains("alacritty")
            || term.contains("direct")
        {
            return Self::TrueColor;
        }
        if term == "dumb" {
            return Self::Mono;
        }
        if term.contains("256") || program == "Apple_Terminal" {
            return Self::Ansi256;
        }
        Self::Ansi16
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TrueColor => "truecolor",
            Self::Ansi256 => "256 colors",
            Self::Ansi16 => "16 colors",
            Self::Mono => "monochrome",
        }
    }
}

macro_rules! tokens {
    ($($name:ident),* $(,)?) => {
        /// Semantic color tokens. Every theme defines all of them.
        #[derive(Debug, Clone, PartialEq)]
        pub struct Palette { $(pub $name: Rgb,)* }
        pub const TOKEN_NAMES: &[&str] = &[$(stringify!($name),)*];
        impl Palette {
            fn set(&mut self, key: &str, v: Rgb) -> bool {
                match key { $(stringify!($name) => { self.$name = v; true })* _ => false }
            }
        }
    };
}

tokens!(
    bg,
    surface,
    text,
    muted,
    primary,
    secondary,
    border,
    selection_bg,
    selection_fg,
    user,
    assistant,
    tool,
    success,
    warning,
    error,
    progress,
    code_bg,
    syn_keyword,
    syn_string,
    syn_comment,
    syn_number,
    syn_function,
    syn_type,
);

#[derive(Debug, Clone)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub description: String,
    pub light: bool,
    pub palette: Palette,
    pub depth: ColorDepth,
}

fn p(hex: &str) -> Rgb {
    Rgb::parse(hex).expect("valid builtin color")
}

#[allow(clippy::too_many_arguments)]
fn palette(
    bg: &str,
    surface: &str,
    text: &str,
    muted: &str,
    primary: &str,
    secondary: &str,
    border: &str,
    sel_bg: &str,
    sel_fg: &str,
    user: &str,
    assistant: &str,
    tool: &str,
    success: &str,
    warning: &str,
    error: &str,
    progress: &str,
    code_bg: &str,
    syn: [&str; 6],
) -> Palette {
    Palette {
        bg: p(bg),
        surface: p(surface),
        text: p(text),
        muted: p(muted),
        primary: p(primary),
        secondary: p(secondary),
        border: p(border),
        selection_bg: p(sel_bg),
        selection_fg: p(sel_fg),
        user: p(user),
        assistant: p(assistant),
        tool: p(tool),
        success: p(success),
        warning: p(warning),
        error: p(error),
        progress: p(progress),
        code_bg: p(code_bg),
        syn_keyword: p(syn[0]),
        syn_string: p(syn[1]),
        syn_comment: p(syn[2]),
        syn_number: p(syn[3]),
        syn_function: p(syn[4]),
        syn_type: p(syn[5]),
    }
}

pub const BUILTIN_IDS: &[&str] = &["phosphor", "amber", "dos", "arcade", "gameboy", "cyber", "paper", "monochrome"];

pub fn builtin(id: &str) -> Option<(String, String, bool, Palette)> {
    let (name, desc, light, pal) = match id {
        "phosphor" => (
            "Phosphor",
            "green CRT glow",
            false,
            palette(
                "#070b07",
                "#0d150d",
                "#8fe38f",
                "#4f7a4f",
                "#b8ffb0",
                "#5fd75f",
                "#2b4a2b",
                "#1f5c1f",
                "#d8ffd0",
                "#c8ffc0",
                "#8fe38f",
                "#6fbf8f",
                "#7dff7d",
                "#d7ff5f",
                "#ff8f6f",
                "#5fff5f",
                "#0a120a",
                ["#d8ffd0", "#9fdf6f", "#4f7a4f", "#bfff9f", "#b8ffb0", "#7fdfaf"],
            ),
        ),
        "amber" => (
            "Amber",
            "warm monochrome terminal",
            false,
            palette(
                "#0f0a04",
                "#18110a",
                "#ffb547",
                "#8a6230",
                "#ffd27f",
                "#ff9f1a",
                "#4a3418",
                "#5a3a10",
                "#fff0c8",
                "#ffe0a0",
                "#ffb547",
                "#d99a4e",
                "#ffd27f",
                "#ffe066",
                "#ff6a3d",
                "#ffc04d",
                "#140e06",
                ["#fff0c8", "#ffcf70", "#8a6230", "#ffe0a0", "#ffd27f", "#f0a860"],
            ),
        ),
        "dos" => (
            "DOS Blue",
            "classic blue utility screens",
            false,
            palette(
                "#0000a8",
                "#00008a",
                "#e8e8e8",
                "#8a8ad8",
                "#55ffff",
                "#ffff55",
                "#5555ff",
                "#00a8a8",
                "#000000",
                "#ffffff",
                "#e8e8e8",
                "#a8ffa8",
                "#55ff55",
                "#ffff55",
                "#ff5555",
                "#55ffff",
                "#000080",
                ["#ffff55", "#55ff55", "#8a8ad8", "#ff55ff", "#55ffff", "#ffffff"],
            ),
        ),
        "arcade" => (
            "Arcade",
            "16-bit cabinet neon",
            false,
            palette(
                "#0b1026",
                "#121a3a",
                "#e6ecff",
                "#6d79a8",
                "#3ee6ff",
                "#ff6f61",
                "#2c3770",
                "#24305f",
                "#ffe66d",
                "#ffe66d",
                "#e6ecff",
                "#7bdcb5",
                "#6dff9e",
                "#ffe66d",
                "#ff4f5e",
                "#3ee6ff",
                "#0e1530",
                ["#ff6f61", "#ffe66d", "#6d79a8", "#c792ea", "#3ee6ff", "#7bdcb5"],
            ),
        ),
        "gameboy" => (
            "Game Boy",
            "four shades of green",
            false,
            palette(
                "#0f380f",
                "#173f17",
                "#9bbc0f",
                "#4f7a28",
                "#c4e05a",
                "#8bac0f",
                "#306230",
                "#306230",
                "#e0f8d0",
                "#e0f8d0",
                "#9bbc0f",
                "#8bac0f",
                "#c4e05a",
                "#e0f8d0",
                "#e0f8d0",
                "#9bbc0f",
                "#0b2e0b",
                ["#e0f8d0", "#c4e05a", "#4f7a28", "#e0f8d0", "#c4e05a", "#8bac0f"],
            ),
        ),
        "cyber" => (
            "Cyber",
            "restrained retro-futurism",
            false,
            palette(
                "#121214",
                "#1a1a1f",
                "#e8e6ef",
                "#6e6a80",
                "#4de1ff",
                "#d36bd6",
                "#36323f",
                "#3a2440",
                "#ffffff",
                "#f3c6ff",
                "#e8e6ef",
                "#8fd8e8",
                "#5cf2a6",
                "#ffd166",
                "#ff5c8a",
                "#4de1ff",
                "#17171c",
                ["#d36bd6", "#9fe7a4", "#6e6a80", "#ffd166", "#4de1ff", "#c3a6ff"],
            ),
        ),
        "paper" => (
            "Paper",
            "vintage manual, light",
            true,
            palette(
                "#f4ecd8",
                "#ebe1c8",
                "#2b2118",
                "#857660",
                "#9a3b1b",
                "#6b4a1e",
                "#c9b896",
                "#d9c9a3",
                "#2b2118",
                "#5a2d10",
                "#2b2118",
                "#4f5d2f",
                "#3d6b2a",
                "#9a6a00",
                "#b0281c",
                "#9a3b1b",
                "#e9dfc4",
                ["#9a3b1b", "#4f5d2f", "#9c8c72", "#7a3e8f", "#6b4a1e", "#2f5d7a"],
            ),
        ),
        "monochrome" => (
            "Monochrome",
            "high-contrast grayscale",
            false,
            palette(
                "#000000",
                "#111111",
                "#e6e6e6",
                "#7a7a7a",
                "#ffffff",
                "#bdbdbd",
                "#4a4a4a",
                "#e6e6e6",
                "#000000",
                "#ffffff",
                "#e6e6e6",
                "#bdbdbd",
                "#ffffff",
                "#d0d0d0",
                "#ffffff",
                "#ffffff",
                "#0d0d0d",
                ["#ffffff", "#bdbdbd", "#6a6a6a", "#d0d0d0", "#ffffff", "#bdbdbd"],
            ),
        ),
        _ => return None,
    };
    Some((name.into(), desc.into(), light, pal))
}

/// Resolve user-facing aliases (`dos-blue`, `game-boy`, `mono`).
pub fn canonical_id(id: &str) -> String {
    let id = id.trim().to_lowercase().replace([' ', '_'], "-");
    match id.as_str() {
        "dos-blue" | "dosblue" | "blue" => "dos".into(),
        "game-boy" | "gb" => "gameboy".into(),
        "mono" | "grayscale" | "greyscale" => "monochrome".into(),
        "green" | "crt" => "phosphor".into(),
        _ => id,
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ThemeError {
    #[error("{file}: {msg}")]
    Invalid { file: String, msg: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    name: String,
    #[serde(default)]
    description: String,
    /// Built-in theme to inherit unspecified tokens from (default: phosphor).
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    light: Option<bool>,
    colors: BTreeMap<String, String>,
}

impl Theme {
    pub fn builtin(id: &str, depth: ColorDepth) -> Option<Self> {
        let id = canonical_id(id);
        let (name, description, light, palette) = builtin(&id)?;
        Some(Self { id, name, description, light, palette, depth })
    }

    /// Parse a custom theme file. Unknown tokens and bad colors are errors.
    pub fn from_toml(id: &str, text: &str, depth: ColorDepth) -> Result<Self, ThemeError> {
        let err = |msg: String| ThemeError::Invalid { file: format!("{id}.toml"), msg };
        let f: ThemeFile = toml::from_str(text).map_err(|e| err(e.message().to_string()))?;
        let base = canonical_id(f.base.as_deref().unwrap_or("phosphor"));
        let (_, _, base_light, mut palette) = builtin(&base).ok_or_else(|| err(format!("unknown base theme '{base}'")))?;
        for (k, v) in &f.colors {
            let rgb = Rgb::parse(v).ok_or_else(|| err(format!("color '{k}' = '{v}' is not #rrggbb")))?;
            if !palette.set(k, rgb) {
                return Err(err(format!("unknown color token '{k}' (valid: {})", TOKEN_NAMES.join(", "))));
            }
        }
        Ok(Self { id: id.into(), name: f.name, description: f.description, light: f.light.unwrap_or(base_light), palette, depth })
    }

    pub fn with_depth(mut self, depth: ColorDepth) -> Self {
        self.depth = depth;
        self
    }

    /// Convert a palette color for the terminal's capabilities.
    pub fn c(&self, rgb: Rgb) -> Color {
        match self.depth {
            ColorDepth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
            ColorDepth::Ansi256 => Color::Indexed(to_256(rgb)),
            ColorDepth::Ansi16 => to_16(rgb),
            ColorDepth::Mono => Color::Reset,
        }
    }

    fn mono(&self) -> bool {
        self.depth == ColorDepth::Mono
    }

    pub fn base(&self) -> Style {
        if self.mono() { Style::default() } else { Style::default().fg(self.c(self.palette.text)).bg(self.c(self.palette.bg)) }
    }
    pub fn fg(&self, rgb: Rgb) -> Style {
        if self.mono() { Style::default() } else { Style::default().fg(self.c(rgb)) }
    }
    pub fn text(&self) -> Style {
        self.fg(self.palette.text)
    }
    pub fn muted(&self) -> Style {
        if self.mono() { Style::default().add_modifier(Modifier::DIM) } else { self.fg(self.palette.muted) }
    }
    pub fn primary(&self) -> Style {
        self.fg(self.palette.primary).add_modifier(Modifier::BOLD)
    }
    pub fn secondary(&self) -> Style {
        self.fg(self.palette.secondary)
    }
    pub fn border(&self) -> Style {
        if self.mono() { Style::default() } else { self.fg(self.palette.border) }
    }
    pub fn border_focus(&self) -> Style {
        self.fg(self.palette.primary)
    }
    pub fn selected(&self) -> Style {
        if self.mono() {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(self.c(self.palette.selection_fg)).bg(self.c(self.palette.selection_bg)).add_modifier(Modifier::BOLD)
        }
    }
    pub fn surface(&self) -> Style {
        if self.mono() { Style::default() } else { Style::default().bg(self.c(self.palette.surface)).fg(self.c(self.palette.text)) }
    }
    pub fn code(&self) -> Style {
        if self.mono() { Style::default() } else { Style::default().bg(self.c(self.palette.code_bg)).fg(self.c(self.palette.text)) }
    }
    pub fn user(&self) -> Style {
        self.fg(self.palette.user)
    }
    pub fn assistant(&self) -> Style {
        self.fg(self.palette.assistant)
    }
    pub fn tool(&self) -> Style {
        self.fg(self.palette.tool)
    }
    pub fn success(&self) -> Style {
        self.fg(self.palette.success)
    }
    pub fn warning(&self) -> Style {
        self.fg(self.palette.warning).add_modifier(if self.mono() { Modifier::BOLD } else { Modifier::empty() })
    }
    pub fn error(&self) -> Style {
        self.fg(self.palette.error).add_modifier(Modifier::BOLD)
    }
    pub fn progress(&self) -> Style {
        self.fg(self.palette.progress)
    }
    /// A dim version of the progress color for the unfilled part of bars.
    pub fn progress_track(&self) -> Style {
        if self.mono() { Style::default().add_modifier(Modifier::DIM) } else { self.fg(self.palette.border) }
    }
    /// Blend between muted and primary; used for pulsing indicators.
    pub fn pulse(&self, t: f32) -> Style {
        self.fg(self.palette.muted.mix(self.palette.primary, t.clamp(0.0, 1.0)))
    }
}

/// Nearest xterm-256 color (6x6x6 cube or grayscale ramp).
pub fn to_256(c: Rgb) -> u8 {
    let level = |v: u8| -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    let steps = [0u8, 95, 135, 175, 215, 255];
    let (r, g, b) = (level(c.0), level(c.1), level(c.2));
    let cube = Rgb(steps[r as usize], steps[g as usize], steps[b as usize]);
    let gray_idx = (((c.0 as u32 + c.1 as u32 + c.2 as u32) / 3).saturating_sub(8) / 10).min(23) as u8;
    let gv = 8 + gray_idx * 10;
    let gray = Rgb(gv, gv, gv);
    let d = |a: Rgb| (a.0 as i32 - c.0 as i32).pow(2) + (a.1 as i32 - c.1 as i32).pow(2) + (a.2 as i32 - c.2 as i32).pow(2);
    if d(gray) < d(cube) { 232 + gray_idx } else { 16 + 36 * r + 6 * g + b }
}

/// Nearest of the 16 standard ANSI colors.
pub fn to_16(c: Rgb) -> Color {
    const TABLE: [(Rgb, Color); 16] = [
        (Rgb(0, 0, 0), Color::Black),
        (Rgb(170, 0, 0), Color::Red),
        (Rgb(0, 170, 0), Color::Green),
        (Rgb(170, 85, 0), Color::Yellow),
        (Rgb(0, 0, 170), Color::Blue),
        (Rgb(170, 0, 170), Color::Magenta),
        (Rgb(0, 170, 170), Color::Cyan),
        (Rgb(170, 170, 170), Color::Gray),
        (Rgb(85, 85, 85), Color::DarkGray),
        (Rgb(255, 85, 85), Color::LightRed),
        (Rgb(85, 255, 85), Color::LightGreen),
        (Rgb(255, 255, 85), Color::LightYellow),
        (Rgb(85, 85, 255), Color::LightBlue),
        (Rgb(255, 85, 255), Color::LightMagenta),
        (Rgb(85, 255, 255), Color::LightCyan),
        (Rgb(255, 255, 255), Color::White),
    ];
    TABLE
        .iter()
        .min_by_key(|(t, _)| (t.0 as i32 - c.0 as i32).pow(2) + (t.1 as i32 - c.1 as i32).pow(2) + (t.2 as i32 - c.2 as i32).pow(2))
        .map(|(_, col)| *col)
        .unwrap()
}

/// Built-in themes plus valid custom themes from `dir`. Invalid custom theme
/// files are reported, not fatal.
pub fn load_all(dir: &Path, depth: ColorDepth) -> (Vec<Theme>, Vec<String>) {
    let mut themes: Vec<Theme> = BUILTIN_IDS.iter().filter_map(|id| Theme::builtin(id, depth)).collect();
    let mut errors = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")).collect();
        paths.sort();
        for path in paths {
            let id = canonical_id(&path.file_stem().unwrap_or_default().to_string_lossy());
            if BUILTIN_IDS.contains(&id.as_str()) {
                errors.push(format!("{}: custom themes can't replace built-in '{id}'", path.display()));
                continue;
            }
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| Theme::from_toml(&id, &t, depth).map_err(|e| e.to_string()))
            {
                Ok(t) => themes.push(t),
                Err(e) => errors.push(e),
            }
        }
    }
    (themes, errors)
}

pub const EXAMPLE_THEME: &str = r##"# Save as <config dir>/themes/<id>.toml, then /theme <id>
name = "Midnight Teal"
description = "my custom palette"
base = "cyber"            # inherit any color not listed below
[colors]
bg = "#081418"
text = "#d7f4f2"
primary = "#3fe0c5"
secondary = "#f2a65a"
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtins_exist_with_distinct_palettes() {
        let themes: Vec<Theme> = BUILTIN_IDS.iter().map(|id| Theme::builtin(id, ColorDepth::TrueColor).unwrap()).collect();
        assert_eq!(themes.len(), 8);
        for (i, a) in themes.iter().enumerate() {
            for b in &themes[i + 1..] {
                assert_ne!(a.palette, b.palette, "{} vs {}", a.id, b.id);
            }
        }
        assert!(Theme::builtin("paper", ColorDepth::TrueColor).unwrap().light);
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(Theme::builtin("DOS Blue", ColorDepth::TrueColor).unwrap().id, "dos");
        assert_eq!(Theme::builtin("game_boy", ColorDepth::TrueColor).unwrap().id, "gameboy");
        assert!(Theme::builtin("nope", ColorDepth::TrueColor).is_none());
    }

    #[test]
    fn custom_theme_inherits_and_validates() {
        let t = Theme::from_toml("teal", EXAMPLE_THEME, ColorDepth::TrueColor).unwrap();
        assert_eq!(t.palette.bg, Rgb(0x08, 0x14, 0x18));
        assert_eq!(t.palette.error, Theme::builtin("cyber", ColorDepth::TrueColor).unwrap().palette.error);
        let bad_color = "name='x'\n[colors]\nbg='green'\n";
        assert!(Theme::from_toml("x", bad_color, ColorDepth::TrueColor).unwrap_err().to_string().contains("#rrggbb"));
        let bad_token = "name='x'\n[colors]\nbackground='#000000'\n";
        assert!(Theme::from_toml("x", bad_token, ColorDepth::TrueColor).unwrap_err().to_string().contains("unknown color token"));
        let bad_base = "name='x'\nbase='nope'\n[colors]\n";
        assert!(Theme::from_toml("x", bad_base, ColorDepth::TrueColor).is_err());
    }

    #[test]
    fn shipped_example_theme_is_valid() {
        let t =
            Theme::from_toml("midnight-teal", include_str!("../../../assets/themes/midnight-teal.toml"), ColorDepth::TrueColor).unwrap();
        assert_eq!(t.name, "Midnight Teal");
    }

    #[test]
    fn load_all_reports_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("teal.toml"), EXAMPLE_THEME).unwrap();
        std::fs::write(dir.path().join("broken.toml"), "name = ").unwrap();
        std::fs::write(dir.path().join("amber.toml"), EXAMPLE_THEME).unwrap();
        let (themes, errors) = load_all(dir.path(), ColorDepth::TrueColor);
        assert_eq!(themes.len(), 9);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn color_degradation() {
        assert_eq!(to_256(Rgb(0, 0, 0)), 16);
        assert_eq!(to_256(Rgb(255, 255, 255)), 231);
        assert_eq!(to_256(Rgb(128, 128, 128)), 244);
        assert_eq!(to_16(Rgb(0, 0, 168)), Color::Blue);
        assert_eq!(to_16(Rgb(250, 250, 250)), Color::White);
        let t = Theme::builtin("phosphor", ColorDepth::Mono).unwrap();
        assert_eq!(t.c(t.palette.primary), Color::Reset);
        assert!(t.selected().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn depth_detection() {
        let env =
            |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(a, _)| *a == k).map(|(_, v)| v.to_string());
        assert_eq!(ColorDepth::from_env(env(&[("COLORTERM", "truecolor")])), ColorDepth::TrueColor);
        assert_eq!(ColorDepth::from_env(env(&[("TERM", "xterm-256color")])), ColorDepth::Ansi256);
        assert_eq!(ColorDepth::from_env(env(&[("TERM", "xterm")])), ColorDepth::Ansi16);
        assert_eq!(ColorDepth::from_env(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")])), ColorDepth::Mono);
        assert_eq!(ColorDepth::from_env(env(&[("TERM", "dumb")])), ColorDepth::Mono);
    }
}
