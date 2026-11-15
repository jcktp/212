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
        preview: None,
    });
    app_has_text(&mut a, "Download 1 GB?");
    key(&mut a, KeyCode::Enter);
    assert!(matches!(a.outbox.last(), Some(Request::Answer { id: 7, choice: Some(0) })));
    a.on_core(Event::Choice { id: 8, title: "t".into(), body: vec![], options: vec!["A".into(), "B".into()], preview: None });
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

#[test]
fn tool_activity_and_diff_render() {
    let (mut a, _t) = app();
    a.on_core(Event::Inference(InferenceState::Generating));
    a.on_core(Event::TurnStart);
    a.on_core(Event::ToolStart { id: "1".into(), tool: "edit_file".into(), title: "edit app.py".into() });
    app_has_text(&mut a, "editing");
    a.on_core(Event::ToolEnd {
        id: "1".into(),
        ok: true,
        summary: "+1 -1".into(),
        detail: Some("--- a/app.py\n+++ b/app.py\n@@ -1,2 +1,2 @@\n-    return 'hello'\n+    return 'hi'".into()),
    });
    a.on_core(Event::Delta(StreamEvent::Text("Changed the greeting.".into())));
    a.on_core(Event::TurnEnd { completion_tokens: 9, elapsed_ms: 900, tokens_per_second: None, cancelled: false });
    a.on_core(Event::Inference(InferenceState::Ready));
    let screen = render(&mut a, 100, 30);
    assert!(screen.contains("edit app.py · +1 -1"), "{screen}");
    assert!(screen.contains("+    return 'hi'"));
    assert!(!screen.contains("+++ b/app.py"), "file headers are hidden");
    assert!(screen.contains("Changed the greeting."));
    // The continuation after a tool call has no second header.
    assert_eq!(screen.matches("◆").count(), 1, "{screen}");
}

#[test]
fn permission_dialog_shows_preview_and_answers() {
    let (mut a, _t) = app();
    a.on_core(Event::Inference(InferenceState::Generating));
    a.on_core(Event::Choice {
        id: 3,
        title: "Allow: run `cargo test`?".into(),
        body: vec!["The model wants to run this:".into()],
        options: vec!["Allow once".into(), "Always allow this command this session".into(), "Deny".into()],
        preview: Some("$ cargo test".into()),
    });
    let screen = render(&mut a, 110, 30);
    assert!(screen.contains("$ cargo test") && screen.contains("waiting for your approval"), "{screen}");
    key(&mut a, KeyCode::Char('3'));
    assert!(matches!(a.outbox.last(), Some(Request::Answer { id: 3, choice: Some(2) })));
    // A dismissed prompt closes without answering.
    a.on_core(Event::Choice { id: 4, title: "x".into(), body: vec![], options: vec!["a".into(), "b".into()], preview: None });
    a.outbox.clear();
    a.on_core(Event::Dismiss { id: 4 });
    assert!(matches!(a.overlay, Overlay::None) && a.outbox.is_empty());
}

fn summary(id: &str, installed: bool, active: bool, spec: Option<&str>) -> common::proto::ModelSummary {
    common::proto::ModelSummary {
        id: id.into(),
        name: id.into(),
        quant: "Q4_K_M".into(),
        arch: "llama".into(),
        size: 2 << 30,
        context_length: None,
        est_memory: Some(3 << 30),
        fit: common::proto::Fit::Comfortable,
        source: if installed { "local file".into() } else { "on disk (Ollama) · no download".into() },
        path: Default::default(),
        capabilities: vec![],
        active,
        installed,
        install_spec: spec.map(String::from),
    }
}

#[test]
fn models_picker_switches_installs_and_removes() {
    let (mut a, _t) = app();
    a.on_core(Event::Models(vec![
        summary("llava-phi3-latest", true, true, None),
        summary("gemma4:12b", false, false, Some("ollama:gemma4:12b")),
    ]));
    let screen = render(&mut a, 100, 30);
    assert!(screen.contains("[loaded]") && screen.contains("[on disk]"), "{screen}");
    // The loaded model is preselected; move to gemma and press Enter.
    key(&mut a, KeyCode::Down);
    key(&mut a, KeyCode::Enter);
    assert!(matches!(a.outbox.last(), Some(Request::UseModel { spec }) if spec == "ollama:gemma4:12b"));
    // Ctrl+D on an installed model asks the runtime to remove it (which confirms).
    a.on_core(Event::Models(vec![summary("llava-phi3-latest", true, true, None)]));
    ctrl(&mut a, 'd');
    assert!(matches!(a.outbox.last(), Some(Request::RemoveModel { id }) if id == "llava-phi3-latest"));
}

#[test]
fn sessions_picker_resumes_and_transcript_replaces_view() {
    let (mut a, _t) = app();
    a.on_core(Event::SessionList {
        title: "Sessions".into(),
        items: vec![common::proto::SessionItem {
            id: "abc123".into(),
            title: "Parser fix".into(),
            detail: "2h ago".into(),
            current: false,
        }],
    });
    key(&mut a, KeyCode::Enter);
    assert!(matches!(a.outbox.last(), Some(Request::ResumeSession { id: Some(id) }) if id == "abc123"));
    a.on_core(Event::Transcript {
        title: "Parser fix · 2h ago".into(),
        items: vec![
            common::proto::TranscriptItem::Summary("- Done: fixed the lexer".into()),
            common::proto::TranscriptItem::User("now the parser".into()),
            common::proto::TranscriptItem::Assistant { text: "Done.".into(), reasoning: None },
        ],
    });
    let screen = render(&mut a, 100, 30);
    assert!(screen.contains("Resumed: Parser fix") && screen.contains("fixed the lexer") && screen.contains("now the parser"), "{screen}");
}

#[test]
fn api_keys_are_masked_and_never_enter_history() {
    let tmp = tempfile::tempdir().unwrap();
    let hist = tmp.path().join("history");
    let mut a = App::new(TuiOptions {
        ui: config::UiConfig { startup_animation: false, ..Default::default() },
        themes_dir: tmp.path().join("themes"),
        history_file: hist.clone(),
        config_path: tmp.path().join("config.toml"),
        first_run: false,
        warnings: vec![],
        cwd: tmp.path().to_path_buf(),
        keyboard_enhanced: false,
    });
    type_str(&mut a, "/key anthropic");
    a.completion = None;
    key(&mut a, KeyCode::Enter);
    type_str(&mut a, "sk-ant-secret");
    let screen = render(&mut a, 100, 30);
    assert!(!screen.contains("sk-ant-secret") && screen.contains("•••••"), "{screen}");
    key(&mut a, KeyCode::Enter);
    assert!(
        matches!(a.outbox.last(), Some(Request::SetCredential { provider, secret }) if provider == "anthropic" && secret == "sk-ant-secret")
    );
    let saved = std::fs::read_to_string(&hist).unwrap_or_default();
    assert!(!saved.contains("sk-ant"), "history must not contain the key: {saved}");
}

#[test]
fn header_shows_stats_and_weather_when_room() {
    let (mut a, _t) = app();
    a.on_core(Event::Snapshot(Snapshot { provider: "local".into(), model: Some("qwen".into()), cwd: "/p".into(), ..Default::default() }));
    a.on_core(Event::Stats { cpu_percent: 23.4, mem_used: 11 << 30, mem_total: 16 << 30, model_mem: Some(3 << 30) });
    a.on_core(Event::Weather(Some("rain 12°C Amsterdam".into())));
    let wide = render(&mut a, 140, 10);
    let header = wide.lines().next().unwrap();
    assert!(header.contains("cpu 23% · ram 11.0G/16.0G · model 3.0G") && header.contains("☂ 12°C Amsterdam"), "{header}");
    let narrow = render(&mut a, 60, 10);
    let header = narrow.lines().next().unwrap();
    assert!(header.contains("local") && header.contains("qwen"), "{header}");
}
