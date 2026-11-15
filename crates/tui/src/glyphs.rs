//! Unicode and ASCII glyph sets. Everything that could render as tofu on a
//! limited terminal goes through here.

use ratatui::symbols::border;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    pub unicode: bool,
}

impl Glyphs {
    pub fn detect(setting: &str) -> Self {
        let unicode = match setting {
            "unicode" => true,
            "ascii" => false,
            _ => Self::env_supports_unicode(|k| std::env::var(k).ok()),
        };
        Self { unicode }
    }

    pub fn env_supports_unicode(get: impl Fn(&str) -> Option<String>) -> bool {
        if get("WT_SESSION").is_some() || get("TERM_PROGRAM").is_some() {
            return true;
        }
        if get("TERM").as_deref() == Some("linux") {
            return false;
        }
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|k| get(k).filter(|v| !v.is_empty()))
            .map(|v| v.to_uppercase().contains("UTF-8") || v.to_uppercase().contains("UTF8"))
            .unwrap_or(cfg!(target_os = "macos"))
    }

    pub fn border(self) -> border::Set<'static> {
        if self.unicode { border::ROUNDED } else { ASCII_BORDER }
    }
    pub fn border_plain(self) -> border::Set<'static> {
        if self.unicode { border::PLAIN } else { ASCII_BORDER }
    }
    pub fn border_double(self) -> border::Set<'static> {
        if self.unicode { border::DOUBLE } else { ASCII_BORDER }
    }
    pub fn bar_full(self) -> &'static str {
        if self.unicode { "█" } else { "#" }
    }
    pub fn bar_empty(self) -> &'static str {
        if self.unicode { "░" } else { "." }
    }
    pub fn bar_partial(self) -> &'static [&'static str] {
        if self.unicode { &["▏", "▎", "▍", "▌", "▋", "▊", "▉"] } else { &["-"] }
    }
    pub fn hrule(self) -> &'static str {
        if self.unicode { "─" } else { "-" }
    }
    pub fn vbar(self) -> &'static str {
        if self.unicode { "│" } else { "|" }
    }
    pub fn quote_bar(self) -> &'static str {
        if self.unicode { "┃" } else { "|" }
    }
    pub fn bullet(self) -> &'static str {
        if self.unicode { "•" } else { "*" }
    }
    pub fn prompt(self) -> &'static str {
        if self.unicode { "▶" } else { ">" }
    }
    pub fn user_mark(self) -> &'static str {
        if self.unicode { "▌" } else { ">" }
    }
    pub fn assistant_mark(self) -> &'static str {
        if self.unicode { "◆" } else { "*" }
    }
    pub fn dot(self) -> &'static str {
        if self.unicode { "●" } else { "o" }
    }
    pub fn dot_empty(self) -> &'static str {
        if self.unicode { "○" } else { "." }
    }
    pub fn check(self) -> &'static str {
        if self.unicode { "✓" } else { "+" }
    }
    pub fn cross(self) -> &'static str {
        if self.unicode { "✗" } else { "x" }
    }
    pub fn warn(self) -> &'static str {
        if self.unicode { "▲" } else { "!" }
    }
    pub fn info(self) -> &'static str {
        if self.unicode { "◇" } else { "-" }
    }
    pub fn arrow(self) -> &'static str {
        if self.unicode { "→" } else { "->" }
    }
    pub fn down(self) -> &'static str {
        if self.unicode { "▼" } else { "v" }
    }
    pub fn ellipsis(self) -> &'static str {
        if self.unicode { "…" } else { "..." }
    }
    pub fn collapsed(self) -> &'static str {
        if self.unicode { "▸" } else { "+" }
    }
    pub fn expanded(self) -> &'static str {
        if self.unicode { "▾" } else { "-" }
    }
    pub fn logo_inline(self) -> &'static str {
        if self.unicode { "◢212◣" } else { "[212]" }
    }
    pub fn sep(self) -> &'static str {
        if self.unicode { " │ " } else { " | " }
    }
}

pub const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_unicode_from_locale() {
        let env =
            |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(a, _)| *a == k).map(|(_, v)| v.to_string());
        assert!(Glyphs::env_supports_unicode(env(&[("LANG", "en_US.UTF-8")])));
        assert!(!Glyphs::env_supports_unicode(env(&[("LANG", "C"), ("TERM", "xterm")])));
        assert!(!Glyphs::env_supports_unicode(env(&[("TERM", "linux"), ("LANG", "en_US.UTF-8")])));
        assert!(Glyphs::env_supports_unicode(env(&[("WT_SESSION", "x")])));
    }
}
