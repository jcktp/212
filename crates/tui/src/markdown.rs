//! A small, streaming-friendly Markdown renderer with word wrapping and
//! lightweight syntax highlighting.
//!
//! It handles what chat models actually emit — headings, lists, quotes, rules,
//! fenced code, inline code/bold/italic/links — and tolerates incomplete input
//! (an unclosed fence mid-stream renders as code). Output lines never exceed
//! the requested width.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::glyphs::Glyphs;
use crate::theme::Theme;

pub struct Ctx<'a> {
    pub theme: &'a Theme,
    pub glyphs: Glyphs,
    pub width: usize,
}

type Seg = (String, Style);

/// Render Markdown into wrapped lines. `prefix` is drawn before every line
/// (e.g. a gutter); its width is subtracted from the available width.
pub fn render(text: &str, cx: &Ctx, base: Style, prefix: &[Span<'static>]) -> Vec<Line<'static>> {
    let t = cx.theme;
    let g = cx.glyphs;
    let pw: usize = prefix.iter().map(|s| s.content.width()).sum();
    let width = cx.width.saturating_sub(pw).max(8);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut code: Option<String> = None;

    for raw in text.split('\n') {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```").or_else(|| trimmed.strip_prefix("~~~")) {
            match code {
                None => {
                    let lang = rest.trim().to_lowercase();
                    let label = if lang.is_empty() { String::new() } else { format!(" {lang} ") };
                    let fill = width.saturating_sub(label.width() + 1);
                    let mut spans = prefix.to_vec();
                    spans.push(Span::styled(g.hrule().to_string(), t.border()));
                    spans.push(Span::styled(label, t.muted()));
                    spans.push(Span::styled(g.hrule().repeat(fill), t.border()));
                    out.push(Line::from(spans));
                    code = Some(lang);
                }
                Some(_) => {
                    let mut spans = prefix.to_vec();
                    spans.push(Span::styled(g.hrule().repeat(width), t.border()));
                    out.push(Line::from(spans));
                    code = None;
                }
            }
            continue;
        }
        if let Some(lang) = &code {
            out.extend(code_line(raw, lang, width, cx, prefix));
            continue;
        }

        // Block-level constructs.
        let indent = raw.len() - trimmed.len();
        if trimmed.is_empty() {
            out.push(Line::from(prefix.to_vec()));
            continue;
        }
        if is_rule(trimmed) {
            let mut spans = prefix.to_vec();
            spans.push(Span::styled(g.hrule().repeat(width), t.border()));
            out.push(Line::from(spans));
            continue;
        }
        if let Some((level, title)) = heading(trimmed) {
            let mut style = t.primary();
            if level == 1 {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            let segs = inline(title, style, t);
            out.extend(wrap(&segs, width, prefix, prefix));
            continue;
        }
        if let Some(q) = trimmed.strip_prefix('>') {
            let mut p = prefix.to_vec();
            p.push(Span::styled(format!("{} ", g.quote_bar()), t.border()));
            let segs = inline(q.trim_start(), t.muted().add_modifier(Modifier::ITALIC), t);
            out.extend(wrap(&segs, cx.width.saturating_sub(pw + 2).max(8) + pw + 2, &p, &p));
            continue;
        }
        if let Some((marker, body)) = list_item(trimmed) {
            let pad = " ".repeat(indent.min(8));
            let bullet = if marker == "-" || marker == "*" || marker == "+" { g.bullet().to_string() } else { marker.to_string() };
            let mut first = prefix.to_vec();
            first.push(Span::raw(pad.clone()));
            first.push(Span::styled(format!("{bullet} "), t.secondary()));
            let mut cont = prefix.to_vec();
            cont.push(Span::raw(" ".repeat(pad.width() + bullet.width() + 1)));
            let segs = inline(body, base, t);
            out.extend(wrap(&segs, cx.width.max(8), &first, &cont));
            continue;
        }
        let segs = inline(trimmed, base, t);
        let mut first = prefix.to_vec();
        if indent > 0 {
            first.push(Span::raw(" ".repeat(indent.min(8))));
        }
        out.extend(wrap(&segs, cx.width.max(8), &first, prefix));
    }
    out
}

/// Render plain text (no Markdown) wrapped with a prefix.
pub fn plain(text: &str, width: usize, style: Style, first: &[Span<'static>], cont: &[Span<'static>]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (i, l) in text.split('\n').enumerate() {
        let p = if i == 0 { first } else { cont };
        out.extend(wrap(&[(l.to_string(), style)], width, p, cont));
    }
    out
}

fn is_rule(s: &str) -> bool {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && (s.chars().all(|c| c == '-') || s.chars().all(|c| c == '*') || s.chars().all(|c| c == '_'))
}

fn heading(s: &str) -> Option<(usize, &str)> {
    let level = s.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&level) && s[level..].starts_with(' ') { Some((level, s[level..].trim())) } else { None }
}

fn list_item(s: &str) -> Option<(&str, &str)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(m) {
            return Some((&s[..1], rest));
        }
    }
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits < 4 {
        let rest = &s[digits..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some((&s[..digits + 1], &rest[2..]));
        }
    }
    None
}

/// Parse inline Markdown into styled segments.
pub fn inline(s: &str, base: Style, t: &Theme) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut bold = false;
    let mut italic = false;
    let style = |bold: bool, italic: bool| {
        let mut st = base;
        if bold {
            st = st.add_modifier(Modifier::BOLD);
        }
        if italic {
            st = st.add_modifier(Modifier::ITALIC);
        }
        st
    };
    let flush = |buf: &mut String, out: &mut Vec<Seg>, st: Style| {
        if !buf.is_empty() {
            out.push((std::mem::take(buf), st));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '`'
            && let Some(end) = chars[i + 1..].iter().position(|&x| x == '`')
        {
            flush(&mut buf, &mut out, style(bold, italic));
            let code: String = chars[i + 1..i + 1 + end].iter().collect();
            out.push((code, t.code().patch(t.secondary())));
            i += end + 2;
            continue;
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut buf, &mut out, style(bold, italic));
            bold = !bold;
            i += 2;
            continue;
        }
        if (c == '*' || c == '_') && chars.get(i + 1) != Some(&c) {
            let prev_alnum = i > 0 && chars[i - 1].is_alphanumeric();
            let next = chars.get(i + 1).copied();
            let opens = !italic && next.is_some_and(|n| !n.is_whitespace()) && !prev_alnum;
            let closes = italic && i > 0 && !chars[i - 1].is_whitespace() && !next.is_some_and(|n| n.is_alphanumeric());
            if opens || closes {
                flush(&mut buf, &mut out, style(bold, italic));
                italic = !italic;
                i += 1;
                continue;
            }
        }
        if c == '['
            && let Some(close) = chars[i..].iter().position(|&x| x == ']')
            && chars.get(i + close + 1) == Some(&'(')
            && let Some(end) = chars[i + close + 1..].iter().position(|&x| x == ')')
        {
            flush(&mut buf, &mut out, style(bold, italic));
            let label: String = chars[i + 1..i + close].iter().collect();
            let url: String = chars[i + close + 2..i + close + 1 + end].iter().collect();
            out.push((label, style(bold, italic).patch(t.secondary()).add_modifier(Modifier::UNDERLINED)));
            out.push((format!(" ({url})"), t.muted()));
            i += close + 2 + end;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out, style(bold, italic));
    out
}

/// Greedy word wrap of styled segments. `width` is the full line width
/// including the prefix.
pub fn wrap(segs: &[Seg], width: usize, first: &[Span<'static>], cont: &[Span<'static>]) -> Vec<Line<'static>> {
    // Tokenize into words and whitespace runs, keeping styles.
    let mut tokens: Vec<(String, Style, bool)> = Vec::new();
    for (text, st) in segs {
        let mut cur = String::new();
        let mut cur_ws = None;
        for ch in text.chars() {
            let ws = ch == ' ' || ch == '\t';
            if cur_ws.is_some_and(|w| w != ws) {
                tokens.push((std::mem::take(&mut cur), *st, cur_ws.unwrap()));
            }
            cur.push(if ch == '\t' { ' ' } else { ch });
            cur_ws = Some(ws);
        }
        if !cur.is_empty() {
            tokens.push((cur, *st, cur_ws.unwrap_or(false)));
        }
    }
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = first.to_vec();
    let mut col: usize = first.iter().map(|s| s.content.width()).sum();
    let cont_w: usize = cont.iter().map(|s| s.content.width()).sum();
    let mut at_line_start = true;
    for (tok, st, ws) in tokens {
        let w = tok.width();
        if ws {
            if at_line_start && !lines.is_empty() {
                continue; // drop leading spaces on continuation lines
            }
            if col + w <= width {
                spans.push(Span::styled(tok, st));
                col += w;
            }
            continue;
        }
        if col + w > width && !at_line_start {
            // Trim trailing whitespace span before breaking.
            if spans.last().is_some_and(|s| s.content.trim().is_empty() && !s.content.is_empty()) {
                spans.pop();
            }
            lines.push(Line::from(std::mem::replace(&mut spans, cont.to_vec())));
            col = cont_w;
        }
        if col + w <= width {
            spans.push(Span::styled(tok, st));
            col += w;
        } else {
            // Hard-break a word longer than the line.
            let mut piece = String::new();
            for ch in tok.chars() {
                let cw = ch.width().unwrap_or(0);
                if col + cw > width && col > cont_w.min(col) {
                    if !piece.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut piece), st));
                    }
                    lines.push(Line::from(std::mem::replace(&mut spans, cont.to_vec())));
                    col = cont_w;
                }
                piece.push(ch);
                col += cw;
            }
            if !piece.is_empty() {
                spans.push(Span::styled(piece, st));
            }
        }
        at_line_start = false;
    }
    lines.push(Line::from(spans));
    lines
}

fn code_line(raw: &str, lang: &str, width: usize, cx: &Ctx, prefix: &[Span<'static>]) -> Vec<Line<'static>> {
    let t = cx.theme;
    let gutter = Span::styled(format!("{} ", cx.glyphs.vbar()), t.border());
    let inner = width.saturating_sub(2).max(4);
    let segs = highlight(&raw.replace('\t', "    "), lang, t);
    // Hard-wrap code by character, never by word.
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = prefix.to_vec();
    spans.push(gutter.clone());
    let mut col = 0;
    for (text, st) in segs {
        let mut piece = String::new();
        for ch in text.chars() {
            let cw = ch.width().unwrap_or(0);
            if col + cw > inner {
                spans.push(Span::styled(std::mem::take(&mut piece), st.patch(t.code())));
                lines.push(Line::from(std::mem::take(&mut spans)));
                spans = prefix.to_vec();
                spans.push(gutter.clone());
                col = 0;
            }
            piece.push(ch);
            col += cw;
        }
        if !piece.is_empty() {
            spans.push(Span::styled(piece, t.code().patch(st)));
        }
    }
    spans.push(Span::styled(" ".repeat(inner.saturating_sub(col)), t.code()));
    lines.push(Line::from(spans));
    lines
}

const KEYWORDS: &[&str] = &[
    "fn",
    "let",
    "mut",
    "pub",
    "struct",
    "enum",
    "impl",
    "trait",
    "use",
    "mod",
    "match",
    "if",
    "else",
    "elif",
    "for",
    "while",
    "loop",
    "return",
    "self",
    "Self",
    "crate",
    "super",
    "async",
    "await",
    "move",
    "where",
    "as",
    "in",
    "ref",
    "const",
    "static",
    "type",
    "dyn",
    "unsafe",
    "true",
    "false",
    "def",
    "class",
    "import",
    "from",
    "not",
    "and",
    "or",
    "is",
    "None",
    "True",
    "False",
    "with",
    "try",
    "except",
    "finally",
    "raise",
    "lambda",
    "yield",
    "pass",
    "break",
    "continue",
    "function",
    "var",
    "extends",
    "export",
    "default",
    "new",
    "this",
    "catch",
    "throw",
    "typeof",
    "instanceof",
    "null",
    "undefined",
    "interface",
    "func",
    "package",
    "range",
    "go",
    "defer",
    "chan",
    "map",
    "select",
    "switch",
    "case",
    "nil",
    "int",
    "char",
    "void",
    "public",
    "private",
    "protected",
    "unsigned",
    "long",
    "include",
    "delete",
    "then",
    "fi",
    "do",
    "done",
    "esac",
    "local",
    "echo",
    "Some",
    "Ok",
    "Err",
    "SELECT",
    "FROM",
    "WHERE",
    "INSERT",
    "UPDATE",
    "CREATE",
    "TABLE",
];

fn comment_markers(lang: &str) -> &'static [&'static str] {
    match lang {
        "python" | "py" | "sh" | "bash" | "zsh" | "shell" | "console" | "toml" | "yaml" | "yml" | "ruby" | "rb" | "perl" | "r" | "make"
        | "makefile" | "dockerfile" | "ini" | "conf" | "nix" => &["#"],
        "sql" | "lua" | "haskell" | "hs" => &["--"],
        "" | "text" | "txt" => &["// ", "# "],
        _ => &["//"],
    }
}

/// Lightweight, line-local syntax highlighting.
pub fn highlight(line: &str, lang: &str, t: &Theme) -> Vec<Seg> {
    let p = &t.palette;
    if matches!(lang, "diff" | "patch") {
        let st = if line.starts_with('+') {
            t.success()
        } else if line.starts_with('-') {
            t.error()
        } else if line.starts_with("@@") {
            t.secondary()
        } else {
            t.text()
        };
        return vec![(line.to_string(), st)];
    }
    let comments = comment_markers(lang);
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<Seg> = Vec::new();
    let mut i = 0;
    let text = t.text();
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        if comments.iter().any(|m| rest.starts_with(m)) && (i == 0 || chars[i - 1].is_whitespace() || !rest.starts_with('#')) {
            out.push((rest, t.fg(p.syn_comment).add_modifier(Modifier::ITALIC)));
            break;
        }
        let c = chars[i];
        if c == '"' || c == '\'' || c == '`' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(chars.len());
            // A lone apostrophe in prose/Rust lifetimes: treat as text.
            if c == '\'' && j >= chars.len() {
                out.push((c.to_string(), text));
                i += 1;
                continue;
            }
            out.push((chars[i..end].iter().collect(), t.fg(p.syn_string)));
            i = end;
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_') {
                j += 1;
            }
            out.push((chars[i..j].iter().collect(), t.fg(p.syn_number)));
            i = j;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let st = if KEYWORDS.contains(&word.as_str()) {
                t.fg(p.syn_keyword).add_modifier(Modifier::BOLD)
            } else if chars.get(j) == Some(&'(') || chars.get(j) == Some(&'!') {
                t.fg(p.syn_function)
            } else if word.chars().next().is_some_and(|c| c.is_uppercase()) {
                t.fg(p.syn_type)
            } else {
                text
            };
            out.push((word, st));
            i = j;
            continue;
        }
        let mut j = i;
        while j < chars.len() && !(chars[j].is_alphanumeric() || "_\"'`".contains(chars[j])) {
            let r: String = chars[j..].iter().collect();
            if comments.iter().any(|m| r.starts_with(m)) {
                break;
            }
            j += 1;
        }
        let j = j.max(i + 1);
        out.push((chars[i..j].iter().collect(), text));
        i = j;
    }
    out
}

/// Extract fenced code blocks (for /copy code).
pub fn code_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut cur: Option<Vec<&str>> = None;
    for line in text.split('\n') {
        let tl = line.trim_start();
        if tl.starts_with("```") || tl.starts_with("~~~") {
            match cur.take() {
                Some(b) => blocks.push(b.join("\n")),
                None => cur = Some(Vec::new()),
            }
        } else if let Some(b) = cur.as_mut() {
            b.push(line);
        }
    }
    if let Some(b) = cur {
        blocks.push(b.join("\n"));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorDepth, Theme};

    fn cx(t: &Theme, width: usize) -> Ctx<'_> {
        Ctx { theme: t, glyphs: Glyphs { unicode: true }, width }
    }

    fn text_of(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wraps_within_width() {
        let t = Theme::builtin("phosphor", ColorDepth::TrueColor).unwrap();
        let md = "This is a fairly long paragraph that will definitely need to wrap across several lines of output.\n\n- a list item that is also quite long and wraps nicely\n1. numbered\n\n```rust\nfn main() { println!(\"hello, a very long string literal that exceeds the width\"); }\n```\n> quoted wisdom";
        for w in [12usize, 20, 33, 80] {
            let lines = render(md, &cx(&t, w), t.text(), &[Span::raw("  ")]);
            for l in &lines {
                assert!(l.width() <= w.max(10), "width {w}: '{}' is {}", text_of(l), l.width());
            }
        }
    }

    #[test]
    fn inline_styles() {
        let t = Theme::builtin("amber", ColorDepth::TrueColor).unwrap();
        let segs = inline("use `cargo` **now** and *maybe* [docs](https://x.y)", t.text(), &t);
        let texts: Vec<&str> = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(texts[0], "use ");
        assert!(texts.contains(&"cargo"));
        assert!(segs.iter().any(|(s, st)| s == "now" && st.add_modifier.contains(Modifier::BOLD)));
        assert!(segs.iter().any(|(s, st)| s == "maybe" && st.add_modifier.contains(Modifier::ITALIC)));
        assert!(segs.iter().any(|(s, _)| s == " (https://x.y)"));
        // snake_case identifiers must not toggle italics.
        let segs = inline("call my_func_name here", t.text(), &t);
        assert!(segs.iter().all(|(_, st)| !st.add_modifier.contains(Modifier::ITALIC)));
    }

    #[test]
    fn unclosed_fence_renders_as_code_while_streaming() {
        let t = Theme::builtin("dos", ColorDepth::TrueColor).unwrap();
        let lines = render("Here:\n```py\nprint('hi')", &cx(&t, 40), t.text(), &[]);
        assert!(text_of(lines.last().unwrap()).contains("print('hi')"));
        assert!(text_of(&lines[1]).contains("py"));
    }

    #[test]
    fn highlights_keywords_strings_comments() {
        let t = Theme::builtin("cyber", ColorDepth::TrueColor).unwrap();
        let segs = highlight("let x = \"s\"; // note", "rust", &t);
        let kw = segs.iter().find(|(s, _)| s == "let").unwrap();
        assert_eq!(kw.1.fg, Some(t.c(t.palette.syn_keyword)));
        assert!(segs.iter().any(|(s, st)| s == "\"s\"" && st.fg == Some(t.c(t.palette.syn_string))));
        assert!(segs.iter().any(|(s, st)| s == "// note" && st.fg == Some(t.c(t.palette.syn_comment))));
        let joined: String = highlight("a = b # c", "python", &t).into_iter().map(|(s, _)| s).collect();
        assert_eq!(joined, "a = b # c");
    }

    #[test]
    fn extracts_code_blocks() {
        assert_eq!(code_blocks("x\n```sh\nls\npwd\n```\ny\n```\nz"), vec!["ls\npwd".to_string(), "z".to_string()]);
    }

    #[test]
    fn hard_breaks_long_words() {
        let lines = wrap(&[("a".repeat(25), Style::default())], 10, &[], &[]);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.width() <= 10));
    }
}
