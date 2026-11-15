//! Runtime behaviour against a deterministic mock provider.

use std::sync::Arc;
use std::time::Duration;

use common::proto::{Event, NoticeLevel, Request};
use common::{AppPaths, Role, StreamEvent};
use providers::ProviderError;
use providers::mock::MockProvider;
use tokio::sync::mpsc;
use two_one_two_runtime::{Runtime, RuntimeOptions};

struct Harness {
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<Event>,
    mock: MockProvider,
    _tmp: tempfile::TempDir,
    cwd: std::path::PathBuf,
}

async fn start(mock: MockProvider) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::rooted(tmp.path());
    paths.ensure().unwrap();
    let cwd = tmp.path().join("ws");
    std::fs::create_dir_all(&cwd).unwrap();
    let (ev_tx, rx) = mpsc::channel(1024);
    let rt = Runtime::new(
        RuntimeOptions {
            config_path: paths.config_file(),
            paths,
            config: config::Config::default(),
            cwd: cwd.clone(),
            autoload: false,
            resume: None,
        },
        ev_tx,
    )
    .unwrap()
    .with_provider(Arc::new(mock.clone()), "mock-model");
    let (tx, req_rx) = mpsc::channel(16);
    tokio::spawn(rt.run(req_rx, false));
    Harness { tx, rx, mock, _tmp: tmp, cwd }
}

/// Collect events until `stop` matches (inclusive) or a timeout.
async fn until(rx: &mut mpsc::Receiver<Event>, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(e)) => {
                let done = stop(&e);
                out.push(e);
                if done {
                    return out;
                }
            }
            _ => panic!("timed out; got {out:?}"),
        }
    }
}

fn text_of(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Delta(StreamEvent::Text(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn streams_replies_and_keeps_history() {
    let mut h = start(MockProvider::text(&["Hello there", "Second answer"])).await;
    h.tx.send(Request::Send { text: "hi".into() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    assert!(ev.iter().any(|e| matches!(e, Event::TurnStart)));
    assert_eq!(text_of(&ev), "Hello there");
    assert!(matches!(ev.last(), Some(Event::TurnEnd { cancelled: false, .. })));

    h.tx.send(Request::Send { text: "again".into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    let reqs = h.mock.requests.lock().unwrap();
    let roles: Vec<Role> = reqs[1].messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::System, Role::User, Role::Assistant, Role::User]);
    assert_eq!(reqs[1].messages[2].content, "Hello there");
    assert_eq!(reqs[1].model, "mock-model");
}

#[tokio::test]
async fn cancel_stops_generation_and_keeps_partial_text() {
    let words: Vec<_> = (0..50).map(|i| Ok(StreamEvent::Text(format!("w{i} ")))).collect();
    let mut mock = MockProvider::new(vec![words, vec![Ok(StreamEvent::Text("next".into()))]]);
    mock.delay = Some(Duration::from_millis(20));
    let mut h = start(mock).await;
    h.tx.send(Request::Send { text: "go".into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::Delta(StreamEvent::Text(t)) if t == "w2 ")).await;
    h.tx.send(Request::Cancel).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    assert!(matches!(ev.last(), Some(Event::TurnEnd { cancelled: true, .. })));
    let streamed: usize = ev.iter().filter(|e| matches!(e, Event::Delta(_))).count();
    assert!(streamed < 20, "generation should stop promptly, streamed {streamed}");

    h.tx.send(Request::Send { text: "more".into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    let reqs = h.mock.requests.lock().unwrap();
    let partial = &reqs[1].messages[2];
    assert_eq!(partial.role, Role::Assistant);
    assert!(partial.content.starts_with("w0 w1 w2"));
}

#[tokio::test]
async fn provider_error_is_reported_and_failed_turn_not_duplicated() {
    let err = ProviderError::Http { provider: "mock".into(), status: 500, message: "refused".into() };
    let mut h = start(MockProvider::new(vec![vec![Err(err)], vec![Ok(StreamEvent::Text("ok".into()))]])).await;
    h.tx.send(Request::Send { text: "first".into() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    assert!(ev.iter().any(|e| matches!(e, Event::Notice { level: NoticeLevel::Error, text } if text.contains("refused"))));
    h.tx.send(Request::Send { text: "retry".into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    let reqs = h.mock.requests.lock().unwrap();
    let users: Vec<&str> = reqs[1].messages.iter().filter(|m| m.role == Role::User).map(|m| m.content.as_str()).collect();
    assert_eq!(users, vec!["retry"]);
}

#[tokio::test]
async fn new_conversation_clears_history() {
    let mut h = start(MockProvider::text(&["one", "two"])).await;
    h.tx.send(Request::Send { text: "a".into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    h.tx.send(Request::NewConversation).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::Cleared)).await;
    h.tx.send(Request::Send { text: "b".into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    assert_eq!(h.mock.requests.lock().unwrap()[1].messages.len(), 2);
}

#[tokio::test]
async fn file_mentions_are_attached() {
    let mut h = start(MockProvider::text(&["seen"])).await;
    std::fs::write(h.cwd.join("notes.md"), "the secret word is plover").unwrap();
    h.tx.send(Request::Send { text: "summarize @notes.md".into() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    assert!(ev.iter().any(|e| matches!(e, Event::Notice { text, .. } if text.starts_with("Attached notes.md"))));
    let reqs = h.mock.requests.lock().unwrap();
    assert!(reqs[0].messages.last().unwrap().content.contains("plover"));
}

#[tokio::test]
async fn concurrent_send_is_rejected_while_generating() {
    let mut slow = MockProvider::text(&["a b c d e f g h"]);
    slow.delay = Some(Duration::from_millis(30));
    let mut h = start(slow).await;
    h.tx.send(Request::Send { text: "1".into() }).await.unwrap();
    h.tx.send(Request::Send { text: "2".into() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    assert!(ev.iter().any(|e| matches!(e, Event::Notice { level: NoticeLevel::Warn, text } if text.contains("Still generating"))));
    assert_eq!(h.mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn shutdown_is_acknowledged() {
    let mut h = start(MockProvider::text(&[])).await;
    h.tx.send(Request::Shutdown).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::ShutdownComplete)).await;
}

#[tokio::test]
async fn unknown_model_and_bad_import_report_errors() {
    let mut h = start(MockProvider::text(&[])).await;
    h.tx.send(Request::LoadModel { id: "nope".into() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::Notice { level: NoticeLevel::Error, .. })).await;
    assert!(matches!(ev.last(), Some(Event::Notice { text, .. }) if text.contains("no installed model")));
    h.tx.send(Request::ImportModel { source: "/definitely/not/here.gguf".into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::Notice { level: NoticeLevel::Error, .. })).await;
}

#[tokio::test]
async fn connection_errors_are_retried_once_output_is_empty() {
    let err = ProviderError::Connect { endpoint: "x".into(), message: "reset".into() };
    let mut h = start(MockProvider::new(vec![vec![Err(err)], vec![Ok(StreamEvent::Text("recovered".into()))]])).await;
    h.tx.send(Request::Send { text: "hi".into() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await;
    assert_eq!(text_of(&ev), "recovered");
    assert!(!ev.iter().any(|e| matches!(e, Event::Notice { level: NoticeLevel::Error, .. })));
}
