//! Rendering. Pure functions of [`App`] state; no I/O.

use common::format;
use common::proto::{DownloadPhase, InferenceState, NoticeLevel, ReportStatus};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::anim::{self, Style as Anim};
use crate::app::{App, Boot, Entry, Overlay};
use crate::art;
use crate::commands::COMMANDS;
use crate::markdown::{self, Ctx};

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::default().style(app.theme.base()), area);
    if let Some(boot) = &app.boot {
        draw_boot(f, app, boot.start.elapsed().as_millis() as u64, area);
        if !app.glyphs.unicode {
            asciify(f.buffer_mut());
        }
        return;
    }
    let input_h = input_height(app, area.width);
    let [header, body, activity, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(f, app, header);
    draw_conversation(f, app, body);
    draw_activity(f, app, activity);
    draw_input(f, app, input);
    draw_status(f, app, status);
    if app.completion.is_some() {
        draw_completion(f, app, input);
    }
    match &app.overlay {
        Overlay::Picker(_) => draw_picker(f, app, area),
        Overlay::Choice(_) => draw_choice(f, app, area),
        Overlay::None => {}
    }
    if !app.glyphs.unicode {
        asciify(f.buffer_mut());
    }
}

/// Final safety net for ASCII terminals: map any remaining non-ASCII symbol
/// (from separators, model output, file names…) to an ASCII stand-in.
pub fn asciify(buf: &mut ratatui::buffer::Buffer) {
    for cell in buf.content.iter_mut() {
        let sym = cell.symbol();
        if sym.is_ascii() {
            continue;
        }
        let rep = match sym.chars().next().unwrap_or('?') {
            '·' | '•' | '∙' => "-",
            '…' => ".",
            '—' | '–' | '─' | '━' => "-",
            '│' | '┃' | '║' => "|",
            '×' => "x",
            '↑' | '▲' => "^",
            '↓' | '▼' => "v",
            '←' => "<",
            '→' | '▶' => ">",
            '★' | '◆' | '◇' => "*",
            '✓' => "+",
            '✗' => "x",
            '“' | '”' | '«' | '»' => "\"",
            '‘' | '’' => "'",
            c if c.is_whitespace() => " ",
            _ => "?",
        };
        cell.set_symbol(rep);
    }
}

fn truncate(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if out.width() + 2 > w {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

fn short_path(p: &str) -> String {
    if let Some(home) = std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned())
        && let Some(rest) = p.strip_prefix(&home)
    {
        return format!("~{rest}");
    }
    p.to_string()
}

// ------------------------------------------------------------------- boot

fn draw_boot(f: &mut Frame, app: &App, ms: u64, area: Rect) {
    let t = &app.theme;
    let g = app.glyphs;
    let mut lines: Vec<Line> = Vec::new();
    if app.ui.ascii_art && area.width >= art::logo_width_full() + 4 && area.height >= 18 {
        for row in art::logo_full(g.unicode) {
            lines.push(Line::from(
                row.into_iter().map(|(s, ink)| Span::styled(s, if ink { t.primary() } else { t.border() })).collect::<Vec<_>>(),
            ));
        }
    } else {
        for row in art::logo_compact(g.unicode) {
            lines.push(Line::styled(row, t.primary()));
        }
    }
    lines.push(Line::default());
    let s = &app.snapshot;
    let backend = s.backend.clone().map(|b| b.to_uppercase()).unwrap_or_else(|| "SCANNING…".into());
    let model = match &app.inference {
        InferenceState::Loading { model, .. } => format!("{} LOADING", model.to_uppercase()),
        _ => s.model.clone().map(|m| m.to_uppercase()).unwrap_or_else(|| "NONE SELECTED".into()),
    };
    let items: [(&str, String, bool); 6] = [
        ("212", format!("INITIALIZING v{}", common::VERSION), true),
        ("SYS", format!("TERMINAL {}x{} · {}", area.width, area.height, app.depth.label().to_uppercase()), true),
        ("CFG", format!("{} THEMES · {}", app.themes.len(), app.theme.name.to_uppercase()), true),
        ("LLM", backend, s.backend.is_some()),
        ("MOD", model, true),
        ("OK ", "SYSTEM ONLINE".into(), true),
    ];
    let visible = (ms / Boot::LINE_MS + 1).min(items.len() as u64) as usize;
    for (tag, text, ok) in items.iter().take(visible) {
        lines.push(Line::from(vec![
            Span::styled(format!("[{tag}] "), if *ok { t.secondary() } else { t.warning() }),
            Span::styled(text.clone(), t.text()),
        ]));
    }
    if visible < items.len() {
        lines.push(Line::styled(app.anim.frame(Anim::Spinner, std::time::Duration::from_millis(ms), 1), t.muted()));
    }
    lines.push(Line::default());
    lines.push(Line::styled("press any key", t.muted()));
    let w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
    let h = lines.len() as u16;
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    f.render_widget(Paragraph::new(lines), Rect::new(x, y, w.min(area.width), h.min(area.height)));
}

// ----------------------------------------------------------------- header

fn status_dot(app: &App) -> (String, Style) {
    let t = &app.theme;
    let g = app.glyphs;
    let el = app.activity_elapsed();
    match &app.inference {
        InferenceState::Offline => (format!("{} offline", g.dot_empty()), t.muted()),
        InferenceState::Ready => (format!("{} ready", g.dot()), t.success()),
        InferenceState::Loading { .. } => (format!("{} loading", app.anim.frame(Anim::Pulse, el, 1)), t.pulse(app.anim.phase(el, 900.0))),
        InferenceState::Generating => (format!("{} busy", app.anim.frame(Anim::Pulse, el, 1)), t.pulse(app.anim.phase(el, 900.0))),
        InferenceState::Error(_) => (format!("{} error", g.cross()), t.error()),
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let g = app.glyphs;
    let s = &app.snapshot;
    let (dot, dot_style) = status_dot(app);
    let right = format!("{}{}{}{}", s.provider, g.sep(), short_path(&s.cwd), g.sep());
    let right_w = right.width() + dot.width() + 1;
    let model = match (&s.model, &app.inference) {
        (_, InferenceState::Loading { model, .. }) => model.clone(),
        (Some(m), _) => match &s.model_detail {
            Some(d) if !app.ui.compact_mode => format!("{m} · {d}"),
            _ => m.clone(),
        },
        (None, _) => "no model".into(),
    };
    let logo = format!(" {} ", g.logo_inline());
    let room = (area.width as usize).saturating_sub(logo.width() + right_w + 3);
    let line = Line::from(vec![
        Span::styled(logo, t.primary()),
        Span::styled(g.vbar().to_string(), t.border()),
        Span::styled(format!(" {}", truncate(&model, room)), t.text()),
    ]);
    f.render_widget(Paragraph::new(line).style(t.surface()), area);
    let r = Line::from(vec![Span::styled(right, t.muted()), Span::styled(format!("{dot} "), dot_style)]);
    let rw = r.width() as u16;
    if rw < area.width {
        f.render_widget(Paragraph::new(r).style(t.surface()), Rect::new(area.right() - rw, area.y, rw, 1));
    }
}

// ----------------------------------------------------------- conversation

fn render_entry(app: &App, entry: &Entry, width: u16) -> Vec<Line<'static>> {
    let t = &app.theme;
    let g = app.glyphs;
    let w = width as usize;
    let cx = Ctx { theme: t, glyphs: g, width: w };
    let mut out: Vec<Line<'static>> = Vec::new();
    match entry {
        Entry::Welcome => {
            out.push(Line::default());
            if app.ui.ascii_art && !app.ui.compact_mode {
                for row in art::logo_compact(g.unicode) {
                    out.push(Line::from(vec![Span::raw("  "), Span::styled(row, t.primary())]));
                }
            } else {
                out.push(Line::from(vec![Span::raw("  "), Span::styled(g.logo_inline().to_string(), t.primary())]));
            }
            out.push(Line::default());
            out.push(Line::from(vec![Span::raw("  "), Span::styled("local-first AI terminal".to_string(), t.secondary())]));
            for tip in [
                ("Enter", "send"),
                ("Alt+Enter", "new line"),
                ("/", "commands"),
                ("@", "attach a file"),
                ("Esc", "stop"),
                ("Ctrl+T", "theme"),
            ] {
                out.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{:<10}", tip.0), t.secondary()),
                    Span::styled(tip.1.to_string(), t.muted()),
                ]));
            }
            out.push(Line::default());
        }
        Entry::Help => {
            out.push(Line::default());
            if app.ui.ascii_art {
                for l in art::HELP.lines(g.unicode) {
                    out.push(Line::from(vec![Span::raw("  "), Span::styled(l.to_string(), t.secondary())]));
                }
            }
            out.push(Line::from(vec![Span::raw("  "), Span::styled("Commands".to_string(), t.primary())]));
            for c in COMMANDS {
                out.extend(markdown::wrap(
                    &[(format!("{} ", c.args), t.muted()), (c.help.to_string(), t.text())],
                    w,
                    &[Span::raw("  "), Span::styled(format!("/{:<11}", c.name), t.secondary())],
                    &[Span::raw(" ".repeat(14))],
                ));
            }
            out.push(Line::default());
            out.push(Line::from(vec![Span::raw("  "), Span::styled("Keys".to_string(), t.primary())]));
            for (k, d) in [
                ("Enter", "send · Alt+Enter / Ctrl+J / Shift+Enter: newline"),
                ("↑ ↓", "history (or move between lines)"),
                ("Tab", "complete /commands and @files"),
                ("Esc", "stop generation, loading or download"),
                ("PgUp PgDn", "scroll · Shift+↑↓ line · Ctrl+End bottom"),
                ("Ctrl+T", "theme picker · Ctrl+O toggle reasoning"),
                ("Ctrl+C", "stop / clear input / quit · Ctrl+D quit"),
            ] {
                out.extend(markdown::wrap(
                    &[(d.to_string(), t.text())],
                    w,
                    &[Span::raw("  "), Span::styled(format!("{k:<11}"), t.secondary())],
                    &[Span::raw(" ".repeat(13))],
                ));
            }
            out.push(Line::default());
        }
        Entry::User(text) => {
            out.push(Line::default());
            let mark = vec![Span::styled(format!(" {} ", g.user_mark()), t.user())];
            out.extend(markdown::plain(text, w, t.user().add_modifier(Modifier::BOLD), &mark, &mark));
        }
        Entry::Assistant { text, reasoning, streaming, stats, show_reasoning, model } => {
            out.push(Line::default());
            out.push(Line::from(vec![
                Span::styled(format!(" {} ", g.assistant_mark()), t.primary()),
                Span::styled(if model.is_empty() { "212".to_string() } else { model.clone() }, t.muted()),
            ]));
            let indent = [Span::raw("   ")];
            if !reasoning.is_empty() {
                let thinking_live = *streaming && text.is_empty();
                let n = reasoning.lines().count();
                if *show_reasoning || thinking_live {
                    let bar = [Span::raw("   "), Span::styled(format!("{} ", g.quote_bar()), t.border())];
                    let shown: String = if thinking_live && !*show_reasoning {
                        // Show only the tail while it streams.
                        let lines: Vec<&str> = reasoning.lines().collect();
                        lines[lines.len().saturating_sub(4)..].join("\n")
                    } else {
                        reasoning.trim().to_string()
                    };
                    out.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(format!("{} reasoning", g.expanded()), t.muted()),
                        Span::styled(" · Ctrl+O".to_string(), t.muted().add_modifier(Modifier::DIM)),
                    ]));
                    out.extend(markdown::plain(&shown, w, t.muted().add_modifier(Modifier::ITALIC), &bar, &bar));
                } else {
                    out.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(format!("{} reasoning ({n} lines) · Ctrl+O", g.collapsed()), t.muted()),
                    ]));
                }
            }
            if !text.is_empty() {
                out.extend(markdown::render(text.trim_start_matches('\n'), &cx, t.assistant(), &indent));
            }
            if *streaming {
                let cursor = if app.anim.moving() && (app.activity_elapsed().as_millis() / 450).is_multiple_of(2) { "▍" } else { " " };
                let cursor = if g.unicode { cursor } else { "_" };
                match out.last_mut() {
                    Some(l) if !text.is_empty() && l.width() < w => l.spans.push(Span::styled(cursor.to_string(), t.primary())),
                    _ => out.push(Line::from(vec![Span::raw("   "), Span::styled(cursor.to_string(), t.primary())])),
                }
            }
            if let Some(s) = stats {
                out.push(Line::from(vec![Span::raw("   "), Span::styled(format!("{} {s}", g.hrule()), t.muted())]));
            }
        }
        Entry::Notice { level, text } => {
            let (icon, st) = match level {
                NoticeLevel::Info => (g.info(), t.muted()),
                NoticeLevel::Success => (g.check(), t.success()),
                NoticeLevel::Warn => (g.warn(), t.warning()),
                NoticeLevel::Error => (g.cross(), t.error()),
            };
            let body = if *level == NoticeLevel::Info { t.muted() } else { t.text() };
            out.push(Line::default());
            out.extend(markdown::plain(text, w, body, &[Span::styled(format!(" {icon} "), st)], &[Span::raw("   ")]));
        }
        Entry::Report { title, art: name, lines } => {
            out.push(Line::default());
            if app.ui.ascii_art
                && let Some(ill) = name.and_then(art::by_name)
            {
                for l in ill.lines(g.unicode) {
                    out.push(Line::from(vec![Span::raw("  "), Span::styled(l.to_string(), t.secondary())]));
                }
            }
            out.push(Line::from(vec![Span::raw("  "), Span::styled(title.clone(), t.primary())]));
            let lw = lines.iter().map(|l| l.label.width()).max().unwrap_or(8).min(16);
            for l in lines {
                let (icon, st) = match l.status {
                    ReportStatus::Ok => (g.check(), t.success()),
                    ReportStatus::Warn => (g.warn(), t.warning()),
                    ReportStatus::Fail => (g.cross(), t.error()),
                    ReportStatus::Info => (g.info(), t.muted()),
                };
                let first =
                    vec![Span::styled(format!("  {icon} "), st), Span::styled(format!("{:<lw$}  ", truncate(&l.label, lw)), t.secondary())];
                out.extend(markdown::wrap(&[(l.detail.clone(), t.text())], w, &first, &[Span::raw(" ".repeat(lw + 6))]));
            }
        }
    }
    out
}

fn draw_conversation(f: &mut Frame, app: &mut App, area: Rect) {
    let width = area.width.saturating_sub(1).max(10);
    // Re-render only entries whose content, width or theme changed.
    let theme_gen = app.theme_gen;
    let mut stale: Vec<usize> = Vec::new();
    for (i, e) in app.entries.iter().enumerate() {
        let ok = matches!(&e.cache, Some((w, tg, v, _)) if *w == width && *tg == theme_gen && *v == e.version);
        let animated = matches!(&e.entry, Entry::Assistant { streaming: true, .. });
        if !ok || animated {
            stale.push(i);
        }
    }
    for i in stale {
        let lines = render_entry(app, &app.entries[i].entry, width);
        let v = app.entries[i].version;
        app.entries[i].cache = Some((width, theme_gen, v, lines));
    }
    let total: usize = app.entries.iter().map(|e| e.cache.as_ref().map_or(0, |c| c.3.len())).sum();
    let h = area.height as usize;
    app.page = h.saturating_sub(2).max(1);
    let max_scroll = total.saturating_sub(h);
    app.scroll = app.scroll.min(max_scroll);
    let end = total - app.scroll;
    let start = end.saturating_sub(h);
    let mut visible: Vec<Line> = Vec::with_capacity(h);
    let mut idx = 0;
    for e in &app.entries {
        let lines = &e.cache.as_ref().unwrap().3;
        if idx + lines.len() <= start {
            idx += lines.len();
            continue;
        }
        for l in lines {
            if idx >= start && idx < end {
                visible.push(l.clone());
            }
            idx += 1;
        }
        if idx >= end {
            break;
        }
    }
    // Bottom-anchor short conversations? No: top-anchored reads naturally.
    f.render_widget(Paragraph::new(visible), Rect::new(area.x, area.y, width, area.height));
    if app.scroll > 0 {
        let label = format!(" ↑ {} more · Ctrl+End ", app.scroll);
        let label = if app.glyphs.unicode { label } else { label.replace('↑', "^") };
        let lw = label.width() as u16;
        if lw < area.width {
            f.render_widget(
                Paragraph::new(Span::styled(label, app.theme.selected())),
                Rect::new(area.right().saturating_sub(lw + 1), area.bottom().saturating_sub(1), lw, 1),
            );
        }
    }
    // Scrollbar track.
    if total > h && area.width > 2 {
        let t = &app.theme;
        let thumb_h = ((h * h) / total).max(1);
        let thumb_top = ((start * h) / total).min(h - thumb_h);
        let x = area.right() - 1;
        for y in 0..h {
            let on = y >= thumb_top && y < thumb_top + thumb_h;
            let (sym, st) = if on {
                (if app.glyphs.unicode { "┃" } else { "#" }, t.secondary())
            } else {
                (if app.glyphs.unicode { "│" } else { "|" }, t.border())
            };
            f.render_widget(Paragraph::new(Span::styled(sym, st)), Rect::new(x, area.y + y as u16, 1, 1));
        }
    }
}

// --------------------------------------------------------------- activity

fn draw_activity(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let g = app.glyphs;
    let el = app.activity_elapsed();
    let w = area.width as usize;
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    if let Some(d) = &app.download {
        let pct = d.total.filter(|t| *t > 0).map(|t| d.downloaded as f64 / t as f64);
        let bar_w = (w / 4).clamp(10, 30);
        spans.push(Span::styled(format!("{} ", g.down()), t.secondary()));
        spans.push(Span::styled(truncate(&d.label, 28), t.text()));
        spans.push(Span::raw(" "));
        match (pct, d.phase) {
            (_, DownloadPhase::Verifying) => {
                spans.push(Span::styled(app.anim.frame(Anim::Scan, el, bar_w), t.progress()));
                spans.push(Span::styled(" verifying checksum".to_string(), t.muted()));
            }
            (Some(p), _) => {
                let (done, rest) = anim::progress_bar(p, bar_w, g.unicode);
                spans.push(Span::styled(done, t.progress()));
                spans.push(Span::styled(rest, t.progress_track()));
                let eta = if d.bytes_per_sec > 1.0 {
                    let remaining = d.total.unwrap_or(0).saturating_sub(d.downloaded) as f64 / d.bytes_per_sec;
                    format!(" · ETA {}", format::duration(std::time::Duration::from_secs_f64(remaining)))
                } else {
                    String::new()
                };
                spans.push(Span::styled(
                    format!(
                        " {:>3.0}% · {} / {} · {}/s{eta} · Esc cancel",
                        p * 100.0,
                        format::bytes(d.downloaded),
                        format::bytes(d.total.unwrap_or(0)),
                        format::bytes(d.bytes_per_sec as u64)
                    ),
                    t.muted(),
                ));
            }
            (None, _) => {
                spans.push(Span::styled(app.anim.frame(Anim::Blocks, el, bar_w.min(12)), t.progress()));
                spans.push(Span::styled(format!(" {} · connecting", format::bytes(d.downloaded)), t.muted()));
            }
        }
    } else {
        match &app.inference {
            InferenceState::Loading { model, detail } => {
                spans.push(Span::styled(app.anim.frame(Anim::Bar, el, 14), t.progress()));
                spans.push(Span::styled(format!(" loading {model} "), t.text()));
                spans.push(Span::styled(format!("· {detail} · {}s · Esc cancel", el.as_secs()), t.muted()));
            }
            InferenceState::Generating => {
                let thinking = matches!(app.entries.last().map(|e| &e.entry), Some(Entry::Assistant { text, reasoning, .. }) if text.is_empty() && !reasoning.is_empty());
                let waiting = app.streamed_tokens == 0;
                let (frame, label) = if waiting {
                    (app.anim.frame(Anim::Bounce, el, 5), "processing prompt".to_string())
                } else if thinking {
                    (app.anim.frame(Anim::Pulse, el, 1), "reasoning".to_string())
                } else {
                    (app.anim.frame(Anim::Spinner, el, 1), "generating".to_string())
                };
                spans.push(Span::styled(frame, t.pulse(app.anim.phase(el, 1200.0)).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(format!(" {label}"), t.text()));
                spans.push(Span::styled(format!(" · {} tok · {:.1}s · Esc stop", app.streamed_tokens, el.as_secs_f64()), t.muted()));
            }
            InferenceState::Error(e) => {
                spans.push(Span::styled(format!("{} ", g.cross()), t.error()));
                spans.push(Span::styled(truncate(e, w.saturating_sub(6)), t.error()));
            }
            _ => {
                if let Some((msg, _)) = &app.toast {
                    spans.push(Span::styled(format!("{} {msg}", g.check()), t.success()));
                }
            }
        }
    }
    let line = Line::from(spans);
    f.render_widget(Paragraph::new(line), area);
}

// ------------------------------------------------------------------ input

fn input_height(app: &App, width: u16) -> u16 {
    let inner = width.saturating_sub(4) as usize;
    let (rows, _) = app.composer.layout(inner);
    (rows.len().clamp(1, 8) + 2) as u16
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let g = app.glyphs;
    let focused = matches!(app.overlay, Overlay::None);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(g.border())
        .border_style(if focused { t.border_focus() } else { t.border() })
        .title(Span::styled(format!(" {} ", app.snapshot.mode.as_str()), t.muted()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let prompt_w = 2u16;
    let text_area = Rect::new(inner.x + prompt_w, inner.y, inner.width.saturating_sub(prompt_w), inner.height);
    f.render_widget(Paragraph::new(Span::styled(format!("{} ", g.prompt()), t.primary())), Rect::new(inner.x, inner.y, prompt_w, 1));
    if app.composer.is_empty() {
        let ph = if app.quitting { "shutting down…" } else { "Ask anything · / commands · @ attach file" };
        f.render_widget(Paragraph::new(Span::styled(ph, t.muted())), text_area);
        if focused {
            f.set_cursor_position(Position::new(text_area.x, text_area.y));
        }
        return;
    }
    let (rows, (cr, cc)) = app.composer.layout(text_area.width as usize);
    let h = text_area.height as usize;
    let top = cr.saturating_sub(h.saturating_sub(1));
    let lines: Vec<Line> = rows.iter().skip(top).take(h).map(|r| Line::styled(r.clone(), t.text())).collect();
    f.render_widget(Paragraph::new(lines), text_area);
    if focused {
        f.set_cursor_position(Position::new(text_area.x + cc as u16, text_area.y + (cr - top) as u16));
    }
}

fn draw_completion(f: &mut Frame, app: &App, input: Rect) {
    let Some(c) = &app.completion else { return };
    let t = &app.theme;
    let n = c.items.len().min(8);
    let top = c.selected.saturating_sub(n - 1);
    let w = (input.width.saturating_sub(4)).min(72);
    let h = n as u16 + 2;
    let area = Rect::new(input.x + 2, input.y.saturating_sub(h), w, h);
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).border_set(app.glyphs.border()).border_style(t.border()).style(t.surface());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let name_w = c.items.iter().map(|i| i.0.width()).max().unwrap_or(8).min(40) + 2;
    let lines: Vec<Line> = c
        .items
        .iter()
        .enumerate()
        .skip(top)
        .take(n)
        .map(|(i, (name, detail))| {
            let sel = i == c.selected;
            let st = if sel { t.selected() } else { t.surface() };
            let text = format!(" {:<name_w$}{}", truncate(name, name_w - 1), detail);
            Line::styled(format!("{:<width$}", truncate(&text, inner.width as usize), width = inner.width as usize), st)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

// ----------------------------------------------------------------- status

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let g = app.glyphs;
    let s = &app.snapshot;
    let mut spans = vec![Span::styled(format!(" {} ", s.mode.as_str().to_uppercase()), t.selected()), Span::raw(" ")];
    if s.context_size > 0 {
        let frac = s.context_used as f64 / s.context_size as f64;
        let (done, rest) = anim::progress_bar(frac, 6, g.unicode);
        let st = if frac > 0.9 {
            t.error()
        } else if frac > 0.7 {
            t.warning()
        } else {
            t.progress()
        };
        spans.push(Span::styled("ctx ", t.muted()));
        spans.push(Span::styled(done, st));
        spans.push(Span::styled(rest, t.progress_track()));
        spans.push(Span::styled(format!(" {}/{}", kfmt(s.context_used), kfmt(s.context_size)), t.muted()));
    }
    if let Some(tps) = app.last_speed {
        spans.push(Span::styled(g.sep().to_string(), t.border()));
        spans.push(Span::styled(format!("{tps:.1} tok/s"), t.secondary()));
    }
    let left_w: usize = spans.iter().map(|s| s.content.width()).sum();
    let hints = if app.busy() { "Esc stop · Ctrl+C cancel" } else { "Enter send · Alt+Enter newline · / commands · Ctrl+C quit" };
    let room = (area.width as usize).saturating_sub(left_w + 2);
    if room > 10 {
        let h = truncate(hints, room);
        spans.push(Span::raw(" ".repeat(room - h.width())));
        spans.push(Span::styled(h, t.muted()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(t.surface()), area);
}

fn kfmt(n: u32) -> String {
    if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() }
}

// --------------------------------------------------------------- overlays

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_picker(f: &mut Frame, app: &App, area: Rect) {
    let Overlay::Picker(p) = &app.overlay else { return };
    let t = &app.theme;
    let g = app.glyphs;
    let visible = p.visible();
    let rows = visible.len().clamp(1, 14) as u16;
    let box_area = centered(area, 78, rows * 2 + 5);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(g.border_double())
        .border_style(t.border_focus())
        .title(Span::styled(format!(" {} ", p.title), t.primary()))
        .title_bottom(Span::styled(format!(" {} ", p.hint), t.muted()))
        .style(t.surface());
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let filter_line = Line::from(vec![
        Span::styled(format!("{} ", g.prompt()), t.primary()),
        Span::styled(
            if p.filter.is_empty() { "type to filter".to_string() } else { p.filter.clone() },
            if p.filter.is_empty() { t.muted() } else { t.text() },
        ),
    ]);
    f.render_widget(Paragraph::new(filter_line), Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1));
    let list_h = inner.height.saturating_sub(2) as usize;
    let per = 2usize;
    let cap = (list_h / per).max(1);
    let top = p.selected.saturating_sub(cap - 1);
    let iw = inner.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (vi, (_, item)) in visible.iter().enumerate().skip(top).take(cap) {
        let sel = vi == p.selected;
        let tag = item.tag.as_ref().map(|t| format!(" [{t}]")).unwrap_or_default();
        let marker = if sel { g.prompt() } else { " " };
        let head = format!("{marker} {}{tag}", truncate(&item.label, iw.saturating_sub(tag.width() + 3)));
        let st = if sel { t.selected() } else { t.text() };
        lines.push(Line::styled(format!("{head:<iw$}"), st));
        lines.push(Line::styled(format!("   {}", truncate(&item.detail, iw.saturating_sub(3))), t.muted()));
    }
    if visible.is_empty() {
        lines.push(Line::styled("  no matches", t.muted()));
    }
    f.render_widget(
        Paragraph::new(lines),
        Rect::new(inner.x + 1, inner.y + 2, inner.width.saturating_sub(2), inner.height.saturating_sub(2)),
    );
}

fn draw_choice(f: &mut Frame, app: &App, area: Rect) {
    let Overlay::Choice(c) = &app.overlay else { return };
    let t = &app.theme;
    let g = app.glyphs;
    let body_w = c.body.iter().map(|l| l.width()).max().unwrap_or(20).max(c.title.width() + 4).min(90) as u16;
    let art_lines: &[&str] =
        if app.ui.ascii_art && c.title.to_lowercase().contains("download") { art::FLOPPY.lines(g.unicode) } else { &[] };
    let h = c.body.len() as u16 + art_lines.len() as u16 + 6;
    let box_area = centered(area, body_w + 6, h);
    f.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(g.border_double())
        .border_style(t.border_focus())
        .title(Span::styled(format!(" {} ", c.title), t.primary()))
        .style(t.surface());
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);
    let mut lines: Vec<Line> = Vec::new();
    for a in art_lines {
        lines.push(Line::styled(format!(" {a}"), t.secondary()));
    }
    lines.push(Line::default());
    for b in &c.body {
        lines.push(Line::styled(format!(" {}", truncate(b, inner.width.saturating_sub(2) as usize)), t.text()));
    }
    lines.push(Line::default());
    let mut buttons: Vec<Span> = vec![Span::raw(" ")];
    for (i, o) in c.options.iter().enumerate() {
        let st = if i == c.selected { t.selected() } else { t.secondary() };
        buttons.push(Span::styled(format!("[ {o} ]"), st));
        buttons.push(Span::raw("  "));
    }
    lines.push(Line::from(buttons));
    lines.push(Line::styled(" ←→ choose · Enter confirm · Esc cancel", t.muted()));
    f.render_widget(Paragraph::new(lines), inner);
}
