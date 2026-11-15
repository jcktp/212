//! UI state and input handling. Rendering lives in `view.rs`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::proto::{
    AgentMode, DownloadPhase, DownloadProgress, Event, Fit, InferenceState, ModelSummary, NoticeLevel, ReportLine, ReportStatus, Request,
    SearchHit, SettingValue, Snapshot,
};
use common::{StreamEvent, format};
use config::{AnimationMode, UiConfig};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::Line;

use crate::anim::Animator;
use crate::commands;
use crate::glyphs::Glyphs;
use crate::input::Composer;
use crate::theme::{self, ColorDepth, Theme};

pub struct TuiOptions {
    pub ui: UiConfig,
    pub themes_dir: PathBuf,
    pub history_file: PathBuf,
    pub config_path: PathBuf,
    pub first_run: bool,
    pub warnings: Vec<String>,
    pub cwd: PathBuf,
    pub keyboard_enhanced: bool,
}

#[derive(Debug, Clone)]
pub enum Entry {
    Welcome,
    Help,
    User(String),
    Assistant { text: String, reasoning: String, streaming: bool, stats: Option<String>, show_reasoning: bool, model: String },
    Notice { level: NoticeLevel, text: String },
    Report { title: String, art: Option<&'static str>, lines: Vec<ReportLine> },
}

pub struct EntryBox {
    pub entry: Entry,
    pub version: u64,
    pub cache: Option<(u16, u64, u64, Vec<Line<'static>>)>,
}

#[derive(Debug, Clone)]
pub struct PickItem {
    pub key: String,
    pub label: String,
    pub detail: String,
    pub tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickerKind {
    Theme { original: String },
    Models,
    Search,
    Provider,
}

pub struct Picker {
    pub kind: PickerKind,
    pub title: String,
    pub hint: String,
    pub items: Vec<PickItem>,
    pub selected: usize,
    pub filter: String,
}

impl Picker {
    pub fn visible(&self) -> Vec<(usize, &PickItem)> {
        let f = self.filter.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, i)| f.is_empty() || i.label.to_lowercase().contains(&f) || i.detail.to_lowercase().contains(&f))
            .collect()
    }
    fn clamp(&mut self) {
        let n = self.visible().len();
        self.selected = if n == 0 { 0 } else { self.selected.min(n - 1) };
    }
    fn current(&self) -> Option<&PickItem> {
        self.visible().get(self.selected).map(|(_, i)| *i)
    }
}

pub struct ChoiceDialog {
    pub id: u64,
    pub title: String,
    pub body: Vec<String>,
    pub options: Vec<String>,
    pub selected: usize,
}

pub enum Overlay {
    None,
    Picker(Picker),
    Choice(ChoiceDialog),
}

pub struct Completion {
    pub start: usize,
    pub items: Vec<(String, String)>,
    pub selected: usize,
}

pub struct Boot {
    pub start: Instant,
}

impl Boot {
    pub const LINE_MS: u64 = 90;
    pub const LINES: u64 = 6;
    pub fn done(&self) -> bool {
        self.start.elapsed() > Duration::from_millis(Self::LINE_MS * Self::LINES + 350)
    }
}

#[derive(PartialEq)]
enum Onboarding {
    Off,
    Theme,
    Recommend,
}

pub struct App {
    pub theme: Theme,
    pub theme_gen: u64,
    pub themes: Vec<Theme>,
    pub depth: ColorDepth,
    pub glyphs: Glyphs,
    pub anim: Animator,
    pub ui: UiConfig,
    pub entries: Vec<EntryBox>,
    pub composer: Composer,
    pub completion: Option<Completion>,
    pub overlay: Overlay,
    pub snapshot: Snapshot,
    pub inference: InferenceState,
    pub download: Option<DownloadProgress>,
    pub scroll: usize,
    pub page: usize,
    pub boot: Option<Boot>,
    pub activity_start: Instant,
    pub last_speed: Option<f64>,
    pub streamed_tokens: u64,
    pub outbox: Vec<Request>,
    pub dirty: bool,
    pub quitting: bool,
    pub exit: bool,
    pub toast: Option<(String, Instant)>,
    pub config_path: PathBuf,
    pub keyboard_enhanced: bool,
    onboarding: Onboarding,
    files: Option<Vec<String>>,
    cwd: PathBuf,
    next_version: u64,
}

impl App {
    pub fn new(opts: TuiOptions) -> Self {
        let depth = ColorDepth::detect(&opts.ui.color_depth);
        let glyphs = Glyphs::detect(&opts.ui.glyphs);
        let (themes, theme_errors) = theme::load_all(&opts.themes_dir, depth);
        let wanted = theme::canonical_id(&opts.ui.theme);
        let mut warnings = opts.warnings;
        warnings.extend(theme_errors);
        let theme = themes.iter().find(|t| t.id == wanted).cloned().unwrap_or_else(|| {
            warnings.push(format!("theme '{}' not found; using phosphor", opts.ui.theme));
            themes[0].clone()
        });
        let anim = Animator { mode: opts.ui.animations, speed: opts.ui.animation_speed, unicode: glyphs.unicode };
        let boot = (opts.ui.startup_animation && opts.ui.animations != AnimationMode::Off).then(|| Boot { start: Instant::now() });
        let mut app = Self {
            theme,
            theme_gen: 0,
            themes,
            depth,
            glyphs,
            anim,
            ui: opts.ui,
            entries: Vec::new(),
            composer: Composer::with_history(&opts.history_file),
            completion: None,
            overlay: Overlay::None,
            snapshot: Snapshot::default(),
            inference: InferenceState::Offline,
            download: None,
            scroll: 0,
            page: 10,
            boot,
            activity_start: Instant::now(),
            last_speed: None,
            streamed_tokens: 0,
            outbox: Vec::new(),
            dirty: true,
            quitting: false,
            exit: false,
            toast: None,
            config_path: opts.config_path,
            keyboard_enhanced: opts.keyboard_enhanced,
            onboarding: if opts.first_run { Onboarding::Theme } else { Onboarding::Off },
            files: None,
            cwd: opts.cwd,
            next_version: 1,
        };
        app.push(Entry::Welcome);
        for w in warnings {
            app.push(Entry::Notice { level: NoticeLevel::Warn, text: w });
        }
        if app.boot.is_none() {
            app.after_boot();
        }
        app
    }

    pub fn push(&mut self, entry: Entry) {
        self.next_version += 1;
        self.entries.push(EntryBox { entry, version: self.next_version, cache: None });
        self.dirty = true;
    }

    fn touch_last(&mut self) {
        self.next_version += 1;
        if let Some(e) = self.entries.last_mut() {
            e.version = self.next_version;
        }
        self.dirty = true;
    }

    fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.push(Entry::Notice { level, text: text.into() });
    }

    fn send(&mut self, r: Request) {
        self.outbox.push(r);
    }

    pub fn busy(&self) -> bool {
        matches!(self.inference, InferenceState::Loading { .. } | InferenceState::Generating)
    }

    /// True while something on screen moves; the event loop ticks only then.
    pub fn animating(&self) -> bool {
        if self.boot.is_some() {
            return true;
        }
        if self.toast.is_some() {
            return true;
        }
        self.anim.moving() && (self.busy() || self.download.is_some())
    }

    pub fn tick(&mut self) {
        if let Some(b) = &self.boot
            && b.done()
        {
            self.boot = None;
            self.after_boot();
        }
        if let Some((_, t)) = &self.toast
            && t.elapsed() > Duration::from_secs(2)
        {
            self.toast = None;
        }
        self.dirty = true;
    }

    fn after_boot(&mut self) {
        if self.onboarding == Onboarding::Theme {
            self.open_theme_picker("Welcome to 212 — choose your look");
        }
    }

    fn set_theme(&mut self, id: &str) -> bool {
        match self.themes.iter().find(|t| t.id == id) {
            Some(t) => {
                self.theme = t.clone();
                self.theme_gen += 1;
                self.dirty = true;
                true
            }
            None => false,
        }
    }

    fn persist(&mut self, section: &str, key: &str, value: SettingValue) {
        self.send(Request::SaveSetting { section: section.into(), key: key.into(), value });
    }

    fn open_theme_picker(&mut self, title: &str) {
        let items: Vec<PickItem> = self
            .themes
            .iter()
            .map(|t| PickItem {
                key: t.id.clone(),
                label: t.name.clone(),
                detail: t.description.clone(),
                tag: (t.id == self.theme.id).then(|| "current".into()),
            })
            .collect();
        let selected = items.iter().position(|i| i.key == self.theme.id).unwrap_or(0);
        self.overlay = Overlay::Picker(Picker {
            kind: PickerKind::Theme { original: self.theme.id.clone() },
            title: title.into(),
            hint: "↑↓ preview · Enter select · Esc cancel".into(),
            items,
            selected,
            filter: String::new(),
        });
        self.dirty = true;
    }

    // ---------------------------------------------------------------- events

    pub fn on_core(&mut self, ev: Event) {
        self.dirty = true;
        match ev {
            Event::Snapshot(s) => self.snapshot = s,
            Event::Inference(s) => {
                if matches!(s, InferenceState::Loading { .. } | InferenceState::Generating) && !self.busy() {
                    self.activity_start = Instant::now();
                }
                self.inference = s;
            }
            Event::TurnStart => {
                self.streamed_tokens = 0;
                self.last_speed = None;
                self.activity_start = Instant::now();
                let model = self.snapshot.model.clone().unwrap_or_default();
                self.push(Entry::Assistant {
                    text: String::new(),
                    reasoning: String::new(),
                    streaming: true,
                    stats: None,
                    show_reasoning: self.ui.show_reasoning,
                    model,
                });
                self.scroll = 0;
            }
            Event::Delta(d) => {
                match d {
                    StreamEvent::Text(t) => {
                        self.streamed_tokens += 1;
                        if let Some(EntryBox { entry: Entry::Assistant { text, .. }, .. }) = self.entries.last_mut() {
                            text.push_str(&t);
                        }
                    }
                    StreamEvent::Reasoning(r) => {
                        self.streamed_tokens += 1;
                        if let Some(EntryBox { entry: Entry::Assistant { reasoning, .. }, .. }) = self.entries.last_mut() {
                            reasoning.push_str(&r);
                        }
                    }
                    StreamEvent::Speed { tokens_per_second } => self.last_speed = Some(tokens_per_second),
                    _ => {}
                }
                self.touch_last();
            }
            Event::TurnEnd { completion_tokens, elapsed_ms, tokens_per_second, cancelled } => {
                if tokens_per_second.is_some() {
                    self.last_speed = tokens_per_second;
                }
                let idx = self.entries.iter().rposition(|e| matches!(e.entry, Entry::Assistant { .. }));
                if let Some(EntryBox { entry: Entry::Assistant { streaming, stats, text, reasoning, .. }, .. }) =
                    idx.map(|i| &mut self.entries[i])
                {
                    *streaming = false;
                    let speed = tokens_per_second.map(|t| format!(" · {t:.1} tok/s")).unwrap_or_default();
                    let secs = elapsed_ms as f64 / 1000.0;
                    *stats = Some(if cancelled {
                        format!("stopped · {completion_tokens} tok · {secs:.1}s")
                    } else {
                        format!("{completion_tokens} tok{speed} · {secs:.1}s")
                    });
                    if text.is_empty() && reasoning.is_empty() {
                        self.entries.remove(idx.unwrap());
                    }
                }
                self.touch_last();
            }
            Event::Notice { level, text } => self.notice(level, text),
            Event::Models(list) => self.open_models_picker(list),
            Event::SearchResults { query, hits } => self.open_search_picker(&query, hits),
            Event::Download(p) => {
                self.download = match p.phase {
                    DownloadPhase::Finished | DownloadPhase::Failed | DownloadPhase::Cancelled => None,
                    _ => {
                        if self.download.is_none() {
                            self.activity_start = Instant::now();
                        }
                        Some(p)
                    }
                }
            }
            Event::Report { title, art, lines } => self.push(Entry::Report { title, art, lines }),
            Event::Choice { id, title, body, options } => {
                self.overlay = Overlay::Choice(ChoiceDialog { id, title, body, options, selected: 0 });
            }
            Event::Cleared => {
                self.entries.clear();
                self.scroll = 0;
                self.notice(NoticeLevel::Info, "New conversation.");
            }
            Event::ShutdownComplete => self.exit = true,
        }
    }

    fn open_models_picker(&mut self, list: Vec<ModelSummary>) {
        if list.is_empty() {
            self.notice(
                NoticeLevel::Info,
                "No models installed yet. /recommend shows models suited to this machine; /models search <name> searches Hugging Face; /models import <file.gguf> or /models import ollama adds existing files.",
            );
            return;
        }
        let g = self.glyphs;
        let items = list
            .into_iter()
            .map(|m| {
                let fit = match m.fit {
                    Fit::Comfortable => g.check(),
                    Fit::Tight => g.warn(),
                    Fit::TooLarge => g.cross(),
                    Fit::Unknown => "?",
                };
                let mem = m.est_memory.map(|e| format!(" · ~{} RAM {fit}", format::bytes(e))).unwrap_or_default();
                let caps = if m.capabilities.is_empty() { String::new() } else { format!(" · {}", m.capabilities.join(",")) };
                PickItem {
                    key: m.id.clone(),
                    label: m.id.clone(),
                    detail: format!("{} · {} · {}{mem}{caps} · {}", m.arch, m.quant, format::bytes(m.size), m.source),
                    tag: m.active.then(|| "active".into()),
                }
            })
            .collect();
        self.overlay = Overlay::Picker(Picker {
            kind: PickerKind::Models,
            title: "Installed models".into(),
            hint: "Enter load · i info · Del remove · type to filter · Esc close".into(),
            items,
            selected: 0,
            filter: String::new(),
        });
    }

    fn open_search_picker(&mut self, query: &str, hits: Vec<SearchHit>) {
        if hits.is_empty() {
            self.notice(NoticeLevel::Info, format!("No GGUF models found for '{query}'."));
            return;
        }
        let items = hits.into_iter().map(|h| PickItem { key: h.spec, label: h.title, detail: h.detail, tag: None }).collect();
        self.overlay = Overlay::Picker(Picker {
            kind: PickerKind::Search,
            title: format!("Models · {query}"),
            hint: "Enter download/import (asks first) · type to filter · Esc close".into(),
            items,
            selected: 0,
            filter: String::new(),
        });
        if self.onboarding == Onboarding::Recommend {
            self.onboarding = Onboarding::Off;
        }
    }

    pub fn on_paste(&mut self, s: &str) {
        match &mut self.overlay {
            Overlay::Picker(p) => {
                p.filter.push_str(s.lines().next().unwrap_or(""));
                p.clamp();
            }
            Overlay::Choice(_) => {}
            Overlay::None => {
                self.composer.insert_str(s);
                self.update_completion();
            }
        }
        self.dirty = true;
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        self.dirty = true;
        if self.boot.take().is_some() {
            self.after_boot();
            if !matches!(k.code, KeyCode::Char(_)) {
                return;
            }
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') && !matches!(self.overlay, Overlay::None) {
            self.close_overlay(true);
            return;
        }
        match &mut self.overlay {
            Overlay::Choice(_) => return self.on_choice_key(k),
            Overlay::Picker(_) => return self.on_picker_key(k),
            Overlay::None => {}
        }
        if self.completion.is_some() && self.on_completion_key(k) {
            return;
        }
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if self.busy() || self.download.is_some() {
                    self.send(Request::Cancel);
                } else if !self.composer.is_empty() {
                    self.composer.clear();
                    self.completion = None;
                } else {
                    self.quit();
                }
            }
            KeyCode::Char('d') if ctrl => {
                if self.composer.is_empty() {
                    self.quit();
                } else {
                    self.composer.delete();
                }
            }
            KeyCode::Char('t') if ctrl => self.open_theme_picker("Theme"),
            KeyCode::Char('o') if ctrl => self.toggle_reasoning(),
            KeyCode::Char('l') if ctrl => self.dirty = true,
            KeyCode::Char('a') if ctrl => self.composer.home(),
            KeyCode::Char('e') if ctrl => self.composer.end(),
            KeyCode::Char('u') if ctrl => self.composer.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.composer.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.composer.delete_word_back(),
            KeyCode::Char('j') if ctrl => self.composer.insert('\n'),
            KeyCode::Char('h') if ctrl => self.composer.backspace(),
            KeyCode::Char('b') if alt => self.composer.word_left(),
            KeyCode::Char('f') if alt => self.composer.word_right(),
            KeyCode::Char(c) if !ctrl => {
                self.composer.insert(c);
                self.update_completion();
            }
            KeyCode::Enter if alt || shift => self.composer.insert('\n'),
            KeyCode::Enter => self.submit(),
            KeyCode::Tab => self.update_completion(),
            KeyCode::Backspace if alt || ctrl => self.composer.delete_word_back(),
            KeyCode::Backspace => {
                self.composer.backspace();
                self.update_completion();
            }
            KeyCode::Delete => self.composer.delete(),
            KeyCode::Left if ctrl || alt => self.composer.word_left(),
            KeyCode::Right if ctrl || alt => self.composer.word_right(),
            KeyCode::Left => self.composer.left(),
            KeyCode::Right => self.composer.right(),
            KeyCode::Home if ctrl => self.scroll = usize::MAX / 2,
            KeyCode::End if ctrl => self.scroll = 0,
            KeyCode::Home => self.composer.home(),
            KeyCode::End => self.composer.end(),
            KeyCode::Up if shift => self.scroll += 1,
            KeyCode::Down if shift => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Up => {
                if !self.composer.up() {
                    self.composer.history_prev();
                }
            }
            KeyCode::Down => {
                if !self.composer.down() {
                    self.composer.history_next();
                }
            }
            KeyCode::PageUp => self.scroll += self.page,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(self.page),
            KeyCode::Esc => {
                if self.busy() || self.download.is_some() {
                    self.send(Request::Cancel);
                } else {
                    self.scroll = 0;
                }
            }
            KeyCode::F(1) => self.push(Entry::Help),
            _ => {}
        }
    }

    fn quit(&mut self) {
        if !self.quitting {
            self.quitting = true;
            self.send(Request::Shutdown);
        }
    }

    fn toggle_reasoning(&mut self) {
        if let Some(i) = self.entries.iter().rposition(|e| matches!(&e.entry, Entry::Assistant { reasoning, .. } if !reasoning.is_empty()))
        {
            if let Entry::Assistant { show_reasoning, .. } = &mut self.entries[i].entry {
                *show_reasoning = !*show_reasoning;
            }
            self.next_version += 1;
            self.entries[i].version = self.next_version;
        }
    }

    fn close_overlay(&mut self, cancelled: bool) {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Picker(Picker { kind: PickerKind::Theme { original }, .. }) if cancelled => {
                self.set_theme(&original);
                self.advance_onboarding();
            }
            Overlay::Choice(c) => self.send(Request::Answer { id: c.id, choice: None }),
            _ => {}
        }
    }

    fn advance_onboarding(&mut self) {
        if self.onboarding == Onboarding::Theme {
            self.onboarding = Onboarding::Recommend;
            self.notice(
                NoticeLevel::Info,
                "212 runs models locally with its built-in llama.cpp backend — private and offline-capable. Cloud providers (/provider) are optional.",
            );
            self.send(Request::Recommend);
        }
    }

    fn on_choice_key(&mut self, k: KeyEvent) {
        let Overlay::Choice(c) = &mut self.overlay else { return };
        let n = c.options.len().max(1);
        match k.code {
            KeyCode::Left | KeyCode::Up | KeyCode::BackTab => c.selected = (c.selected + n - 1) % n,
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => c.selected = (c.selected + 1) % n,
            KeyCode::Char(d) if d.is_ascii_digit() && (d as usize - '0' as usize) >= 1 && (d as usize - '0' as usize) <= n => {
                c.selected = d as usize - '1' as usize;
                let (id, choice) = (c.id, c.selected);
                self.overlay = Overlay::None;
                self.send(Request::Answer { id, choice: Some(choice) });
            }
            KeyCode::Char('y') => {
                let id = c.id;
                self.overlay = Overlay::None;
                self.send(Request::Answer { id, choice: Some(0) });
            }
            KeyCode::Char('n') => self.close_overlay(true),
            KeyCode::Enter => {
                let (id, choice) = (c.id, c.selected);
                self.overlay = Overlay::None;
                self.send(Request::Answer { id, choice: Some(choice) });
            }
            KeyCode::Esc => self.close_overlay(true),
            _ => {}
        }
    }

    fn on_picker_key(&mut self, k: KeyEvent) {
        let Overlay::Picker(p) = &mut self.overlay else { return };
        let n = p.visible().len();
        let mut preview = false;
        match k.code {
            KeyCode::Up => {
                if n > 0 {
                    p.selected = (p.selected + n - 1) % n;
                }
                preview = true;
            }
            KeyCode::Down | KeyCode::Tab => {
                if n > 0 {
                    p.selected = (p.selected + 1) % n;
                }
                preview = true;
            }
            KeyCode::PageUp => {
                p.selected = p.selected.saturating_sub(8);
                preview = true;
            }
            KeyCode::PageDown => {
                p.selected = (p.selected + 8).min(n.saturating_sub(1));
                preview = true;
            }
            KeyCode::Esc => return self.close_overlay(true),
            KeyCode::Enter => return self.picker_accept(),
            KeyCode::Char('i') if p.kind == PickerKind::Models && p.filter.is_empty() => {
                if let Some(item) = p.current() {
                    let id = item.key.clone();
                    self.overlay = Overlay::None;
                    self.send(Request::ModelInfo { id });
                }
                return;
            }
            KeyCode::Delete if p.kind == PickerKind::Models => {
                if let Some(item) = p.current() {
                    let id = item.key.clone();
                    self.overlay = Overlay::None;
                    self.send(Request::RemoveModel { id });
                }
                return;
            }
            KeyCode::Backspace => {
                p.filter.pop();
                p.clamp();
                preview = true;
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                p.filter.push(c);
                p.selected = 0;
                p.clamp();
                preview = true;
            }
            _ => {}
        }
        if preview
            && matches!(p.kind, PickerKind::Theme { .. })
            && let Some(id) = p.current().map(|i| i.key.clone())
        {
            self.set_theme(&id);
        }
    }

    fn picker_accept(&mut self) {
        let Overlay::Picker(p) = std::mem::replace(&mut self.overlay, Overlay::None) else { return };
        let Some(item) = p.current().cloned() else {
            if let PickerKind::Theme { original } = &p.kind {
                self.set_theme(original);
            }
            return;
        };
        match p.kind {
            PickerKind::Theme { .. } => {
                self.set_theme(&item.key);
                self.persist("ui", "theme", SettingValue::Str(item.key.clone()));
                self.toast(format!("Theme: {}", item.label));
                self.advance_onboarding();
            }
            PickerKind::Models => self.send(Request::LoadModel { id: item.key }),
            PickerKind::Search => self.send(Request::Pull { spec: item.key }),
            PickerKind::Provider => self.send(Request::SetProvider { provider: item.key, model: None }),
        }
    }

    fn toast(&mut self, s: String) {
        self.toast = Some((s, Instant::now()));
    }

    // ------------------------------------------------------------ completion

    fn update_completion(&mut self) {
        let Some((start, tok)) = self.composer.completion_token() else {
            self.completion = None;
            return;
        };
        let items: Vec<(String, String)> = if let Some(prefix) = tok.strip_prefix('/') {
            if self.composer.text().contains(' ') {
                Vec::new()
            } else {
                commands::complete(prefix)
                    .into_iter()
                    .map(|c| (format!("/{}", c.name), format!("{} {}", c.args, c.help).trim().to_string()))
                    .collect()
            }
        } else {
            let q = tok.trim_start_matches('@').to_lowercase();
            let files = self.files.get_or_insert_with(|| list_files(&self.cwd));
            let mut m: Vec<&String> = files.iter().filter(|f| f.to_lowercase().contains(&q)).collect();
            m.sort_by_key(|f| (!f.to_lowercase().starts_with(&q), f.len()));
            m.into_iter().take(50).map(|f| (format!("@{f}"), String::new())).collect()
        };
        self.completion = if items.is_empty() { None } else { Some(Completion { start, items, selected: 0 }) };
    }

    /// Returns true if the key was consumed by the completion popup.
    fn on_completion_key(&mut self, k: KeyEvent) -> bool {
        let Some(c) = &mut self.completion else { return false };
        let n = c.items.len();
        match k.code {
            KeyCode::Up => c.selected = (c.selected + n - 1) % n,
            KeyCode::Down => c.selected = (c.selected + 1) % n,
            KeyCode::Tab | KeyCode::Enter => {
                let (start, item) = (c.start, c.items[c.selected].0.clone());
                let exact = self.composer.completion_token().is_some_and(|(_, t)| t == item);
                self.completion = None;
                if k.code == KeyCode::Enter && exact {
                    self.submit();
                    return true;
                }
                let takes_args = commands::COMMANDS.iter().any(|c| format!("/{}", c.name) == item && !c.args.is_empty());
                let suffix = if item.starts_with('@') || takes_args || k.code == KeyCode::Tab { " " } else { "" };
                self.composer.replace_token(start, &format!("{item}{suffix}"));
                if k.code == KeyCode::Enter && !takes_args && !item.starts_with('@') {
                    self.submit();
                }
            }
            KeyCode::Esc => self.completion = None,
            _ => return false,
        }
        true
    }

    // -------------------------------------------------------------- commands

    fn submit(&mut self) {
        let text = self.composer.text();
        if text.trim().is_empty() {
            return;
        }
        self.composer.take();
        self.completion = None;
        self.scroll = 0;
        if let Some(cmd) = commands::parse(&text) {
            self.run_command(cmd);
            return;
        }
        let msg = text.strip_prefix("//").map(|s| format!("/{s}")).unwrap_or(text);
        self.push(Entry::User(msg.clone()));
        self.send(Request::Send { text: msg });
    }

    fn run_command(&mut self, c: commands::Parsed) {
        let arg = |i: usize| c.args.get(i).cloned().unwrap_or_default();
        let after = |n: usize| c.args.iter().skip(n).cloned().collect::<Vec<_>>().join(" ");
        match c.name.as_str() {
            "help" => self.push(Entry::Help),
            "quit" => self.quit(),
            "new" => self.send(Request::NewConversation),
            "clear" => {
                self.entries.clear();
                self.scroll = 0;
            }
            "theme" => {
                if c.args.is_empty() {
                    self.open_theme_picker("Theme");
                } else {
                    let id = theme::canonical_id(&c.rest);
                    if self.set_theme(&id) {
                        self.persist("ui", "theme", SettingValue::Str(id));
                        self.toast(format!("Theme: {}", self.theme.name));
                    } else {
                        let names: Vec<String> = self.themes.iter().map(|t| t.id.clone()).collect();
                        self.notice(NoticeLevel::Error, format!("Unknown theme '{}'. Available: {}", c.rest, names.join(", ")));
                    }
                }
            }
            "animations" => {
                let mode = match arg(0).as_str() {
                    "on" => Some(AnimationMode::On),
                    "off" => Some(AnimationMode::Off),
                    "reduced" => Some(AnimationMode::Reduced),
                    _ => None,
                };
                match mode {
                    Some(m) => {
                        self.anim.mode = m;
                        self.ui.animations = m;
                        self.persist("ui", "animations", SettingValue::Str(arg(0)));
                        self.toast(format!("Animations: {}", arg(0)));
                    }
                    None => {
                        let cur = match self.anim.mode {
                            AnimationMode::On => "on",
                            AnimationMode::Reduced => "reduced",
                            AnimationMode::Off => "off",
                        };
                        self.notice(NoticeLevel::Info, format!("Animations are {cur}. Use /animations on|reduced|off."));
                    }
                }
            }
            "mode" => match AgentMode::parse(&arg(0)) {
                Some(m) => self.send(Request::SetMode(m)),
                None => self.notice(NoticeLevel::Info, format!("Mode is {}. Use /mode chat|plan|build.", self.snapshot.mode.as_str())),
            },
            "models" => match arg(0).as_str() {
                "" | "list" | "ls" => self.send(Request::ListModels),
                "search" | "find" => self.send(Request::Search { query: after(1) }),
                "pull" | "download" | "get" => self.send(Request::Pull { spec: after(1) }),
                "import" | "add" => self.send(Request::ImportModel { source: c.rest.trim_start_matches(&arg(0)).trim().to_string() }),
                "remove" | "rm" | "delete" => self.send(Request::RemoveModel { id: after(1) }),
                "info" | "show" => self.send(Request::ModelInfo { id: after(1) }),
                "switch" | "use" | "load" => {
                    if c.args.len() > 1 {
                        self.send(Request::LoadModel { id: after(1) });
                    } else {
                        self.send(Request::ListModels);
                    }
                }
                "cancel" => self.send(Request::CancelDownload),
                "benchmark" | "bench" => self.notice(
                    NoticeLevel::Info,
                    "Benchmarking is planned. Meanwhile, the tok/s shown after each reply is measured by the backend.",
                ),
                other => self.notice(
                    NoticeLevel::Error,
                    format!("Unknown /models subcommand '{other}'. Try search, pull, import, remove, info, switch."),
                ),
            },
            "model" => {
                if c.args.is_empty() || arg(0) == "switch" && c.args.len() == 1 {
                    if arg(0) == "switch" {
                        self.send(Request::ListModels);
                    } else {
                        self.send(Request::ModelInfo { id: String::new() });
                    }
                } else {
                    let id = if arg(0) == "switch" { after(1) } else { c.rest.clone() };
                    if self.snapshot.provider == "local" {
                        self.send(Request::LoadModel { id });
                    } else {
                        self.send(Request::SetProvider { provider: self.snapshot.provider.clone(), model: Some(id) });
                    }
                }
            }
            "recommend" => self.send(Request::Recommend),
            "provider" => {
                if c.args.is_empty() {
                    let items = [
                        ("local", "212 Local", "built-in llama.cpp · private · offline"),
                        ("ollama", "Ollama", "connect to a running Ollama server (optional)"),
                        ("openai", "OpenAI", "needs OPENAI_API_KEY"),
                        ("anthropic", "Anthropic", "needs ANTHROPIC_API_KEY"),
                        ("compat", "OpenAI-compatible", "providers.compat_url in config"),
                    ]
                    .into_iter()
                    .map(|(k, l, d)| PickItem {
                        key: k.into(),
                        label: l.into(),
                        detail: d.into(),
                        tag: (self.snapshot.provider == k).then(|| "current".into()),
                    })
                    .collect::<Vec<_>>();
                    let selected = items.iter().position(|i| i.tag.is_some()).unwrap_or(0);
                    self.overlay = Overlay::Picker(Picker {
                        kind: PickerKind::Provider,
                        title: "Provider".into(),
                        hint: "Enter switch · Esc close".into(),
                        items,
                        selected,
                        filter: String::new(),
                    });
                } else {
                    let model = c.args.get(1).cloned();
                    self.send(Request::SetProvider { provider: arg(0), model });
                }
            }
            "context" | "compact" => {
                if c.name == "compact" {
                    self.notice(NoticeLevel::Info, "Automatic compaction is planned. Today, older turns are left out (and you're told) when the window fills; /new starts fresh.");
                }
                self.send(Request::Context);
            }
            "status" => self.send(Request::Status),
            "doctor" => {
                let terminal = self.terminal_report();
                self.send(Request::Doctor { terminal });
            }
            "unload" => self.send(Request::UnloadModel),
            "backend" => self.send(Request::InstallBackend),
            "copy" => self.copy(arg(0) == "code"),
            "settings" | "config" => {
                let lines = vec![
                    ReportLine { status: ReportStatus::Info, label: "Config file".into(), detail: self.config_path.display().to_string() },
                    ReportLine { status: ReportStatus::Info, label: "Theme".into(), detail: format!("{} (/theme)", self.theme.id) },
                    ReportLine {
                        status: ReportStatus::Info,
                        label: "Animations".into(),
                        detail: format!("{:?} × {} (/animations)", self.anim.mode, self.anim.speed).to_lowercase(),
                    },
                    ReportLine {
                        status: ReportStatus::Info,
                        label: "Colors".into(),
                        detail: format!("{} · glyphs {}", self.depth.label(), if self.glyphs.unicode { "unicode" } else { "ascii" }),
                    },
                    ReportLine {
                        status: ReportStatus::Info,
                        label: "Custom themes".into(),
                        detail: "drop <id>.toml files into the themes/ folder next to config.toml".into(),
                    },
                    ReportLine {
                        status: ReportStatus::Info,
                        label: "Edit".into(),
                        detail: "open the file in your editor; restart to apply non-UI settings".into(),
                    },
                ];
                self.push(Entry::Report { title: "Settings".into(), art: None, lines });
            }
            other => self.notice(NoticeLevel::Error, format!("Unknown command /{other}. Type /help.")),
        }
    }

    fn terminal_report(&self) -> Vec<ReportLine> {
        let (w, h) = crossterm::terminal::size().unwrap_or((0, 0));
        let term = std::env::var("TERM").unwrap_or_default();
        let prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
        vec![
            ReportLine {
                status: if w >= 60 && h >= 16 { ReportStatus::Ok } else { ReportStatus::Warn },
                label: "Terminal".into(),
                detail: format!(
                    "{w}×{h} · {}{}",
                    if prog.is_empty() { term } else { prog },
                    if std::env::var_os("SSH_CONNECTION").is_some() { " · over SSH" } else { "" }
                ),
            },
            ReportLine {
                status: if self.depth == ColorDepth::TrueColor { ReportStatus::Ok } else { ReportStatus::Info },
                label: "Colors".into(),
                detail: format!("{} · {}", self.depth.label(), if self.glyphs.unicode { "unicode glyphs" } else { "ASCII glyphs" }),
            },
            ReportLine {
                status: ReportStatus::Info,
                label: "Keyboard".into(),
                detail: if self.keyboard_enhanced {
                    "enhanced (Shift+Enter works)".into()
                } else {
                    "standard (use Alt+Enter or Ctrl+J for newline)".into()
                },
            },
        ]
    }

    fn copy(&mut self, code: bool) {
        let last = self.entries.iter().rev().find_map(|e| match &e.entry {
            Entry::Assistant { text, .. } if !text.is_empty() => Some(text.clone()),
            _ => None,
        });
        let Some(text) = last else {
            return self.notice(NoticeLevel::Info, "Nothing to copy yet.");
        };
        let payload = if code {
            match crate::markdown::code_blocks(&text).pop() {
                Some(c) => c,
                None => return self.notice(NoticeLevel::Info, "The last reply has no code block."),
            }
        } else {
            text
        };
        use std::io::Write;
        let seq = format!("\x1b]52;c;{}\x07", base64(payload.as_bytes()));
        let mut out = std::io::stdout();
        if out.write_all(seq.as_bytes()).and_then(|_| out.flush()).is_ok() {
            self.toast(format!("Copied {} via OSC 52", if code { "code" } else { "reply" }));
        }
    }

    pub fn activity_elapsed(&self) -> Duration {
        self.activity_start.elapsed()
    }
}

fn list_files(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root).max_depth(Some(10)).hidden(true).git_ignore(true).build();
    for e in walker.flatten().take(20_000) {
        if e.file_type().is_some_and(|t| t.is_file())
            && let Ok(rel) = e.path().strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
            if out.len() >= 5000 {
                break;
            }
        }
    }
    out.sort();
    out
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}
