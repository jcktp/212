//! UI behaviour: rendering every theme, key handling, overlays and streaming.

use common::StreamEvent;
use common::proto::{Event, InferenceState, Request, SettingValue, Snapshot};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use two_one_two_tui::app::{App, Overlay, TuiOptions};
use two_one_two_tui::theme::BUILTIN_IDS;
use two_one_two_tui::view;

fn app_with(f: impl FnOnce(&mut config::UiConfig)) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut ui =
        config::UiConfig { startup_animation: false, color_depth: "truecolor".into(), glyphs: "unicode".into(), ..Default::default() };
    f(&mut ui);
    let app = App::new(TuiOptions {
        ui,
        themes_dir: tmp.path().join("themes"),
        history_file: tmp.path().join("history"),
        config_path: tmp.path().join("config.toml"),
        first_run: false,
        warnings: vec![],
        cwd: tmp.path().to_path_buf(),
        keyboard_enhanced: false,
    });
    (app, tmp)
}

fn app() -> (App, tempfile::TempDir) {
    app_with(|_| {})
}

fn render(app: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| view::draw(f, app)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..h {
        for x in 0..w {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}
fn ctrl(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}
fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, KeyCode::Char(c));
    }
}

#[test]
fn every_theme_renders_at_every_size() {
    for id in BUILTIN_IDS {
        let (mut a, _t) = app_with(|ui| ui.theme = id.to_string());
        assert_eq!(a.theme.id, *id);
        a.on_core(Event::TurnStart);
        a.on_core(Event::Delta(StreamEvent::Text("Hello **there**\n```rust\nfn main() {}\n```".into())));
        for (w, h) in [(120, 40), (80, 24), (40, 12), (20, 6), (8, 4)] {
            let screen = render(&mut a, w, h);
            if w >= 40 {
                assert!(screen.contains("212"), "{id} {w}x{h}");
            }
        }
    }
}

#[test]
fn theme_picker_previews_and_restores() {
    let (mut a, _t) = app();
    assert_eq!(a.theme.id, "phosphor");
    ctrl(&mut a, 't');
    assert!(matches!(a.overlay, Overlay::Picker(_)));
    key(&mut a, KeyCode::Down);
    assert_eq!(a.theme.id, "amber", "highlight previews immediately");
    key(&mut a, KeyCode::Esc);
    assert_eq!(a.theme.id, "phosphor", "escape restores");
    assert!(a.outbox.is_empty());

    ctrl(&mut a, 't');
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Enter);
    assert_eq!(a.theme.id, "dos");
    assert!(
        a.outbox.iter().any(|r| matches!(r, Request::SaveSetting { key, value: SettingValue::Str(v), .. } if key == "theme" && v == "dos"))
    );
}

#[test]
fn theme_command_switches_by_name_and_alias() {
    let (mut a, _t) = app();
    type_str(&mut a, "/theme game boy");
    key(&mut a, KeyCode::Enter);
    assert_eq!(a.theme.id, "gameboy");
    type_str(&mut a, "/theme nonsense");
    key(&mut a, KeyCode::Enter);
    assert_eq!(a.theme.id, "gameboy");
}

#[test]
fn typing_and_sending_a_message() {
    let (mut a, _t) = app();
    type_str(&mut a, "hello 212");
    app_has_text(&mut a, "hello 212");
    key(&mut a, KeyCode::Enter);
    assert!(a.composer.is_empty());
    assert!(matches!(a.outbox.last(), Some(Request::Send { text }) if text == "hello 212"));
    // Alt+Enter inserts a newline instead of sending.
    type_str(&mut a, "line1");
    a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    type_str(&mut a, "line2");
    assert_eq!(a.composer.text(), "line1\nline2");
}

fn app_has_text(a: &mut App, s: &str) {
    let screen = render(a, 100, 30);
    assert!(screen.contains(s), "screen missing '{s}':\n{screen}");
}

#[test]
fn streaming_reply_renders_and_finishes() {
    let (mut a, _t) = app();
    a.on_core(Event::Inference(InferenceState::Generating));
    a.on_core(Event::TurnStart);
    for w in ["The ", "answer ", "is ", "**42**."] {
        a.on_core(Event::Delta(StreamEvent::Text(w.into())));
    }
    app_has_text(&mut a, "The answer is 42.");
    app_has_text(&mut a, "generating");
    a.on_core(Event::TurnEnd { completion_tokens: 4, elapsed_ms: 500, tokens_per_second: Some(55.5), cancelled: false });
    a.on_core(Event::Inference(InferenceState::Ready));
    app_has_text(&mut a, "4 tok · 55.5 tok/s");
}

#[test]
fn slash_commands_map_to_requests() {
    let (mut a, _t) = app();
    for (input, check) in [
        (
            "/models search qwen coder",
            Box::new(|r: &Request| matches!(r, Request::Search { query } if query == "qwen coder")) as Box<dyn Fn(&Request) -> bool>,
        ),
        ("/models pull ollama:qwen3:0.6b", Box::new(|r: &Request| matches!(r, Request::Pull { spec } if spec == "ollama:qwen3:0.6b"))),
        ("/models import ~/m/x y.gguf", Box::new(|r: &Request| matches!(r, Request::ImportModel { source } if source == "~/m/x y.gguf"))),
        ("/provider anthropic", Box::new(|r: &Request| matches!(r, Request::SetProvider { provider, .. } if provider == "anthropic"))),
        ("/mode plan", Box::new(|r: &Request| matches!(r, Request::SetMode(common::proto::AgentMode::Plan)))),
        ("/new", Box::new(|r: &Request| matches!(r, Request::NewConversation))),
        ("/doctor", Box::new(|r: &Request| matches!(r, Request::Doctor { terminal } if !terminal.is_empty()))),
    ] {
        a.outbox.clear();
        type_str(&mut a, input);
        a.completion = None;
        key(&mut a, KeyCode::Enter);
        assert!(a.outbox.last().is_some_and(&check), "{input}: {:?}", a.outbox);
    }
}

#[test]
fn ctrl_c_cancels_when_busy_and_quits_when_idle() {
    let (mut a, _t) = app();
    a.on_core(Event::Inference(InferenceState::Generating));
    ctrl(&mut a, 'c');
    assert!(matches!(a.outbox.last(), Some(Request::Cancel)));
    a.on_core(Event::Inference(InferenceState::Ready));
    type_str(&mut a, "draft");
    ctrl(&mut a, 'c');
    assert!(a.composer.is_empty(), "first Ctrl+C clears the draft");
    ctrl(&mut a, 'c');
    assert!(a.quitting && matches!(a.outbox.last(), Some(Request::Shutdown)));
}

#[test]
fn choice_dialog_answers() {
    let (mut a, _t) = app();
    a.on_core(Event::Choice {
        id: 7,
        title: "Download 1 GB?".into(),
        body: vec!["x".into()],
        options: vec!["Download".into(), "Cancel".into()],
    });
    app_has_text(&mut a, "Download 1 GB?");
    key(&mut a, KeyCode::Enter);
    assert!(matches!(a.outbox.last(), Some(Request::Answer { id: 7, choice: Some(0) })));
    a.on_core(Event::Choice { id: 8, title: "t".into(), body: vec![], options: vec!["A".into(), "B".into()] });
    key(&mut a, KeyCode::Esc);
    assert!(matches!(a.outbox.last(), Some(Request::Answer { id: 8, choice: None })));
}

#[test]
fn ascii_mode_renders_only_ascii() {
    let (mut a, _t) = app_with(|ui| {
        ui.glyphs = "ascii".into();
        ui.ascii_art = true;
    });
    a.on_core(Event::Snapshot(Snapshot { provider: "local".into(), model: Some("m".into()), ..Default::default() }));
    a.on_core(Event::TurnStart);
    a.on_core(Event::Delta(StreamEvent::Text("- item\n```\ncode\n```".into())));
    a.on_core(Event::TurnEnd { completion_tokens: 1, elapsed_ms: 1, tokens_per_second: None, cancelled: false });
    let screen = render(&mut a, 90, 40);
    let bad: String = screen.chars().filter(|c| !c.is_ascii()).collect();
    assert!(bad.is_empty(), "non-ASCII glyphs in ASCII mode: {bad:?}\n{screen}");
}

#[test]
fn monochrome_depth_uses_no_colors() {
    let (mut a, _t) = app_with(|ui| ui.color_depth = "mono".into());
    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| view::draw(f, &mut a)).unwrap();
    let buf = term.backend().buffer();
    for cell in buf.content() {
        assert!(matches!(cell.fg, Color::Reset) && matches!(cell.bg, Color::Reset), "{cell:?}");
    }
}

#[test]
fn idle_app_does_not_animate() {
    let (mut a, _t) = app();
    assert!(!a.animating(), "nothing should tick while idle");
    a.on_core(Event::Inference(InferenceState::Generating));
    assert!(a.animating());
    let (mut r, _t2) = app_with(|ui| ui.animations = config::AnimationMode::Reduced);
    r.on_core(Event::Inference(InferenceState::Generating));
    assert!(!r.animating(), "reduced motion never ticks");
}

#[test]
fn slash_completion_popup() {
    let (mut a, _t) = app();
    type_str(&mut a, "/the");
    let c = a.completion.as_ref().expect("popup");
    assert_eq!(c.items[0].0, "/theme");
    key(&mut a, KeyCode::Tab);
    assert_eq!(a.composer.text(), "/theme ");
}
