//! The agent loop with scripted tool calls.

use std::sync::Arc;
use std::time::Duration;

use common::proto::{AgentMode, Event, NoticeLevel, Request};
use common::{AppPaths, Role, StreamEvent};
use providers::mock::MockProvider;
use tokio::sync::mpsc;
use two_one_two_runtime::{Runtime, RuntimeOptions};

struct H {
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<Event>,
    mock: MockProvider,
    ws: std::path::PathBuf,
    logs: std::path::PathBuf,
    _tmp: tempfile::TempDir,
}

fn call(id: &str, name: &str, args: &str) -> Result<StreamEvent, providers::ProviderError> {
    Ok(StreamEvent::ToolCallDelta { index: 0, id: Some(id.into()), name: Some(name.into()), arguments: args.into() })
}
fn text(t: &str) -> Result<StreamEvent, providers::ProviderError> {
    Ok(StreamEvent::Text(t.into()))
}

async fn start(turns: Vec<Vec<Result<StreamEvent, providers::ProviderError>>>, mode: AgentMode, max_steps: u32) -> H {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::rooted(tmp.path());
    paths.ensure().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("app.py"), "def greet():\n    return 'hello'\n").unwrap();
    let mut config = config::Config::default();
    config.agent.max_steps = max_steps;
    config.agent.default_mode = mode.as_str().into();
    let (ev_tx, rx) = mpsc::channel(1024);
    let mock = MockProvider::new(turns);
    let logs = paths.log_dir();
    let rt = Runtime::new(RuntimeOptions { config_path: paths.config_file(), paths, config, cwd: ws.clone(), autoload: false }, ev_tx)
        .unwrap()
        .with_provider(Arc::new(mock.clone()), "mock");
    let (tx, req_rx) = mpsc::channel(16);
    tokio::spawn(rt.run(req_rx, false));
    H { tx, rx, mock, ws, logs, _tmp: tmp }
}

/// Run until TurnEnd, answering each Choice with `answer(title) -> option index`.
async fn turn(h: &mut H, msg: &str, answer: impl Fn(&str, &[String]) -> usize) -> Vec<Event> {
    h.tx.send(Request::Send { text: msg.into() }).await.unwrap();
    let mut out = Vec::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), h.rx.recv()).await.expect("timeout").unwrap();
        if let Event::Choice { id, title, options, .. } = &e {
            let pick = answer(title, options);
            h.tx.send(Request::Answer { id: *id, choice: Some(pick) }).await.unwrap();
        }
        let done = matches!(e, Event::TurnEnd { .. });
        out.push(e);
        if done {
            return out;
        }
    }
}

fn tool_ends(ev: &[Event]) -> Vec<(bool, String)> {
    ev.iter().filter_map(|e| if let Event::ToolEnd { ok, summary, .. } = e { Some((*ok, summary.clone())) } else { None }).collect()
}

#[tokio::test]
async fn reads_run_without_asking_and_results_feed_back() {
    let mut h =
        start(vec![vec![call("c1", "read_file", r#"{"path":"app.py"}"#)], vec![text("It returns hello.")]], AgentMode::Plan, 10).await;
    let ev = turn(&mut h, "what does greet return?", |_, _| panic!("reads must not ask")).await;
    assert_eq!(tool_ends(&ev), vec![(true, "2 lines".to_string())]);
    let reqs = h.mock.requests.lock().unwrap();
    assert!(reqs[0].tools.iter().all(|t| t["function"]["name"] != "edit_file"), "plan mode offers read-only tools");
    let second = &reqs[1].messages;
    let tool_msg = second.iter().find(|m| m.role == Role::Tool).unwrap();
    assert_eq!(tool_msg.tool_call_id.as_deref(), Some("c1"));
    assert!(tool_msg.content.contains("return 'hello'"));
    assert!(second.iter().any(|m| m.role == Role::Assistant && m.tool_calls.len() == 1));
}

#[tokio::test]
async fn approved_edit_changes_the_file_with_diff_preview() {
    let edit = r#"{"path":"app.py","old_string":"'hello'","new_string":"'hi'"}"#;
    let mut h = start(vec![vec![call("e1", "edit_file", edit)], vec![text("Done.")]], AgentMode::Build, 10).await;
    let ev = turn(&mut h, "say hi instead", |title, opts| {
        assert!(title.contains("edit app.py"));
        assert_eq!(opts.len(), 3);
        0
    })
    .await;
    let preview = ev.iter().find_map(|e| if let Event::Choice { preview, .. } = e { preview.clone() } else { None }).unwrap();
    assert!(preview.contains("-    return 'hello'") && preview.contains("+    return 'hi'"));
    assert_eq!(std::fs::read_to_string(h.ws.join("app.py")).unwrap(), "def greet():\n    return 'hi'\n");
    assert_eq!(tool_ends(&ev), vec![(true, "+1 -1".to_string())]);
    let audit = std::fs::read_to_string(h.logs.join("audit.jsonl")).unwrap();
    assert!(audit.contains("\"decision\":\"allowed\"") && audit.contains("edit_file"));
}

#[tokio::test]
async fn denied_edit_leaves_file_untouched_and_tells_the_model() {
    let edit = r#"{"path":"app.py","old_string":"'hello'","new_string":"'bye'"}"#;
    let mut h = start(vec![vec![call("e1", "edit_file", edit)], vec![text("OK, I won't.")]], AgentMode::Build, 10).await;
    let ev = turn(&mut h, "change it", |_, opts| opts.len() - 1).await;
    assert!(std::fs::read_to_string(h.ws.join("app.py")).unwrap().contains("'hello'"));
    assert!(!tool_ends(&ev)[0].0);
    let reqs = h.mock.requests.lock().unwrap();
    let result = reqs[1].messages.iter().find(|m| m.role == Role::Tool).unwrap();
    assert!(result.content.contains("not run") && result.content.contains("declined"));
}

#[tokio::test]
async fn session_grant_skips_later_prompts() {
    let e1 = r#"{"path":"app.py","old_string":"'hello'","new_string":"'a'"}"#;
    let e2 = r#"{"path":"app.py","old_string":"'a'","new_string":"'b'"}"#;
    let mut h =
        start(vec![vec![call("1", "edit_file", e1)], vec![call("2", "edit_file", e2)], vec![text("done")]], AgentMode::Build, 10).await;
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a2 = asked.clone();
    turn(&mut h, "go", move |_, _| {
        a2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        1 // allow all edits this session
    })
    .await;
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(std::fs::read_to_string(h.ws.join("app.py")).unwrap().contains("'b'"));
}

#[tokio::test]
async fn plan_mode_refuses_writes_even_if_the_model_tries() {
    let w = r#"{"path":"new.txt","content":"x"}"#;
    let mut h = start(vec![vec![call("w", "write_file", w)], vec![text("ok")]], AgentMode::Plan, 10).await;
    let ev = turn(&mut h, "write", |_, _| panic!("must not ask in plan mode")).await;
    assert!(!h.ws.join("new.txt").exists());
    assert!(tool_ends(&ev)[0].1.contains("not allowed in plan mode"));
}

#[tokio::test]
async fn chat_mode_offers_no_tools() {
    let mut h = start(vec![vec![text("hi")]], AgentMode::Chat, 10).await;
    turn(&mut h, "hello", |_, _| 0).await;
    assert!(h.mock.requests.lock().unwrap()[0].tools.is_empty());
}

#[tokio::test]
async fn invalid_arguments_become_tool_errors() {
    let mut h = start(vec![vec![call("x", "read_file", r#"{"file":"app.py"}"#)], vec![text("sorry")]], AgentMode::Plan, 10).await;
    let ev = turn(&mut h, "read", |_, _| 0).await;
    assert!(tool_ends(&ev)[0].1.contains("missing field"));
    let reqs = h.mock.requests.lock().unwrap();
    assert!(reqs[1].messages.iter().any(|m| m.role == Role::Tool && m.content.starts_with("Error: invalid arguments")));
}

#[tokio::test]
async fn step_limit_stops_runaway_loops() {
    let turns = (0..10).map(|i| vec![call(&format!("c{i}"), "list_dir", "{}")]).collect();
    let mut h = start(turns, AgentMode::Plan, 3).await;
    let ev = turn(&mut h, "loop forever", |_, _| 0).await;
    assert_eq!(h.mock.requests.lock().unwrap().len(), 3);
    assert!(ev.iter().any(|e| matches!(e, Event::Notice { level: NoticeLevel::Warn, text } if text.contains("Stopped after 3 steps"))));
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_kills_a_running_command() {
    let cmd = r#"{"command":"sleep 30"}"#;
    let mut h = start(vec![vec![call("r", "run_command", cmd)], vec![text("never")]], AgentMode::Build, 10).await;
    h.tx.send(Request::Send { text: "run".into() }).await.unwrap();
    let start = std::time::Instant::now();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(10), h.rx.recv()).await.unwrap().unwrap();
        match e {
            Event::Choice { id, .. } => {
                h.tx.send(Request::Answer { id, choice: Some(0) }).await.unwrap();
                let tx = h.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    tx.send(Request::Cancel).await.unwrap();
                });
            }
            Event::ToolStart { .. } => {}
            Event::ToolEnd { ok, summary, .. } => {
                assert!(!ok && summary.contains("cancelled"), "{summary}");
            }
            Event::TurnEnd { cancelled, .. } => {
                assert!(cancelled);
                break;
            }
            _ => {}
        }
    }
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(h.mock.requests.lock().unwrap().len(), 1, "no further model calls after cancel");
}

#[tokio::test]
async fn cancel_while_waiting_for_approval_dismisses_the_prompt() {
    let w = r#"{"path":"n.txt","content":"x"}"#;
    let mut h = start(vec![vec![call("w", "write_file", w)]], AgentMode::Build, 10).await;
    h.tx.send(Request::Send { text: "write".into() }).await.unwrap();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(5), h.rx.recv()).await.unwrap().unwrap();
        match e {
            Event::Choice { .. } => h.tx.send(Request::Cancel).await.unwrap(),
            Event::Dismiss { .. } => {}
            Event::TurnEnd { cancelled, .. } => {
                assert!(cancelled);
                break;
            }
            _ => {}
        }
    }
    assert!(!h.ws.join("n.txt").exists());
}
