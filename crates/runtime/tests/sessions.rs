//! Sessions: persistence across restarts, resume, history, export, compaction.

use std::sync::Arc;
use std::time::Duration;

use common::proto::{Event, NoticeLevel, Request, TranscriptItem};
use common::{AppPaths, Role};
use providers::mock::MockProvider;
use tokio::sync::mpsc;
use two_one_two_runtime::{Runtime, RuntimeOptions};

struct H {
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<Event>,
    mock: MockProvider,
}

fn start(root: &std::path::Path, mock: MockProvider, cfg: impl FnOnce(&mut config::Config), resume: Option<Option<String>>) -> H {
    let paths = AppPaths::rooted(root);
    paths.ensure().unwrap();
    let cwd = root.join("ws");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut config = config::Config::default();
    cfg(&mut config);
    let (ev_tx, rx) = mpsc::channel(1024);
    let rt = Runtime::new(RuntimeOptions { config_path: paths.config_file(), paths, config, cwd, autoload: false, resume }, ev_tx)
        .unwrap()
        .with_provider(Arc::new(mock.clone()), "mock");
    let (tx, req_rx) = mpsc::channel(16);
    tokio::spawn(rt.run(req_rx, false));
    H { tx, rx, mock }
}

async fn until(rx: &mut mpsc::Receiver<Event>, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut out = Vec::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap_or_else(|_| panic!("timeout; got {out:?}")).unwrap();
        let done = stop(&e);
        out.push(e);
        if done {
            return out;
        }
    }
}

async fn say(h: &mut H, text: &str) -> Vec<Event> {
    h.tx.send(Request::Send { text: text.into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::TurnEnd { .. })).await
}

async fn shutdown(mut h: H) {
    h.tx.send(Request::Shutdown).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::ShutdownComplete)).await;
}

#[tokio::test]
async fn conversations_persist_and_resume_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = start(tmp.path(), MockProvider::text(&["Paris is the capital.", "About 2.1 million."]), |_| {}, None);
    say(&mut h, "What is the capital of France?").await;
    say(&mut h, "How many people live there?").await;
    shutdown(h).await;

    // A new process: --continue restores the history and the model sees it.
    let mut h = start(tmp.path(), MockProvider::text(&["Yes."]), |_| {}, Some(None));
    let ev = until(&mut h.rx, |e| matches!(e, Event::Transcript { .. })).await;
    let Some(Event::Transcript { title, items }) = ev.last() else { unreachable!() };
    assert!(title.starts_with("What is the capital of France?"), "{title}");
    assert_eq!(items.len(), 4);
    assert_eq!(items[0], TranscriptItem::User("What is the capital of France?".into()));
    say(&mut h, "Thanks!").await;
    let msgs = h.mock.requests.lock().unwrap()[0].messages.clone();
    let contents: Vec<&str> = msgs.iter().filter(|m| m.role != Role::System).map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        vec!["What is the capital of France?", "Paris is the capital.", "How many people live there?", "About 2.1 million.", "Thanks!"]
    );

    // Listing, search and rename.
    h.tx.send(Request::ListSessions { all: false }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::SessionList { .. })).await;
    let Some(Event::SessionList { items, .. }) = ev.last() else { unreachable!() };
    assert_eq!(items.len(), 1);
    assert!(items[0].current && items[0].detail.contains("6 messages"), "{:?}", items[0]);
    h.tx.send(Request::SearchHistory { query: "capital".into() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::SessionList { .. })).await;
    let Some(Event::SessionList { items, .. }) = ev.last() else { unreachable!() };
    assert_eq!(items.len(), 1);
    h.tx.send(Request::RenameSession { title: "France facts".into() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::Notice { level: NoticeLevel::Success, .. })).await;

    // Export to Markdown.
    h.tx.send(Request::Export { format: "md".into(), path: Some("out.md".into()) }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::Notice { level: NoticeLevel::Success, .. })).await;
    let md = std::fs::read_to_string(tmp.path().join("ws/out.md")).unwrap();
    assert!(md.starts_with("# France facts") && md.contains("## You\n\nThanks!"), "{md}");
    shutdown(h).await;
}

#[tokio::test]
async fn new_conversation_starts_a_separate_session_and_delete_confirms() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = start(tmp.path(), MockProvider::text(&["a", "b"]), |_| {}, None);
    say(&mut h, "first topic").await;
    h.tx.send(Request::NewConversation).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::Cleared)).await;
    say(&mut h, "second topic").await;
    h.tx.send(Request::ListSessions { all: true }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::SessionList { .. })).await;
    let Some(Event::SessionList { items, .. }) = ev.last() else { unreachable!() };
    assert_eq!(items.len(), 2);
    let first = items.iter().find(|i| i.title == "first topic").unwrap().id.clone();
    h.tx.send(Request::DeleteSession { id: first[..6].to_string() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::Choice { .. })).await;
    let Some(Event::Choice { id, title, .. }) = ev.last() else { unreachable!() };
    assert!(title.contains("first topic"));
    h.tx.send(Request::Answer { id: *id, choice: Some(0) }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::Notice { text, .. } if text == "Session deleted.")).await;
    shutdown(h).await;
}

#[tokio::test]
async fn compaction_summarizes_older_turns_and_keeps_recent_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let mock = MockProvider::text(&["one", "two", "three", "- Instructions: be brief\n- Done: counted", "four"]);
    let mut h = start(tmp.path(), mock, |_| {}, None);
    for q in ["q1", "q2", "q3"] {
        say(&mut h, q).await;
    }
    h.tx.send(Request::Compact).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::Notice { level: NoticeLevel::Success, .. })).await;
    assert!(ev.iter().any(
        |e| matches!(e, Event::Transcript { items, .. } if matches!(&items[0], TranscriptItem::Summary(s) if s.contains("be brief")))
    ));
    say(&mut h, "q4").await;
    let reqs = h.mock.requests.lock().unwrap();
    let summary_req = &reqs[3];
    assert!(summary_req.tools.is_empty());
    assert!(summary_req.messages[0].content.contains("Summarize the conversation"));
    let last = &reqs[4].messages;
    assert!(last[0].content.contains("Summary of the earlier part") && last[0].content.contains("be brief"));
    let users: Vec<&str> = last.iter().filter(|m| m.role == Role::User).map(|m| m.content.as_str()).collect();
    assert_eq!(users, vec!["q2", "q3", "q4"], "the last two exchanges stay verbatim");
}

#[tokio::test]
async fn auto_compaction_runs_when_the_window_fills() {
    let tmp = tempfile::tempdir().unwrap();
    let long = "word ".repeat(400); // ~500 tokens per reply
    let replies: Vec<String> = (0..10).map(|i| format!("{i} {long}")).collect();
    let turns: Vec<&str> = replies.iter().map(String::as_str).collect();
    let mut h = start(tmp.path(), MockProvider::text(&turns), |c| c.inference.context_size = 2048, None);
    let mut saw_notice = false;
    for i in 0..5 {
        let ev = say(&mut h, &format!("question {i}")).await;
        saw_notice |= ev.iter().any(|e| matches!(e, Event::Notice { text, .. } if text.starts_with("Compacting")));
    }
    assert!(saw_notice, "auto-compaction should announce itself");
    let reqs = h.mock.requests.lock().unwrap();
    let idx = reqs.iter().position(|r| r.messages[0].content.contains("Summarize the conversation")).expect("a summary request");
    let after = &reqs[idx + 1];
    assert!(after.messages[0].content.contains("Summary of the earlier part"));
    // The message that triggered compaction is sent right after the summary.
    assert!(after.messages.last().unwrap().content.starts_with("question"));
    // No request ever exceeded the window budget silently.
    assert!(reqs.iter().all(|r| r.messages.iter().map(|m| m.content.len()).sum::<usize>() / 4 < 2048));
}

#[tokio::test]
async fn removing_a_model_asks_first() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = start(tmp.path(), MockProvider::text(&[]), |_| {}, None);
    let gguf = tmp.path().join("tiny.gguf");
    std::fs::write(&gguf, models::gguf::testutil::build(&[("general.architecture", models::gguf::testutil::W::Str("llama"))])).unwrap();
    h.tx.send(Request::ImportModel { source: gguf.display().to_string() }).await.unwrap();
    until(&mut h.rx, |e| matches!(e, Event::Notice { level: NoticeLevel::Success, .. })).await;
    h.tx.send(Request::ListModels).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::Models(_))).await;
    let Some(Event::Models(list)) = ev.last() else { unreachable!() };
    let m = list.iter().find(|m| m.installed).unwrap().clone();
    h.tx.send(Request::RemoveModel { id: m.id.clone() }).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::Choice { .. })).await;
    let Some(Event::Choice { id, body, .. }) = ev.last() else { unreachable!() };
    assert!(body[0].contains("original file elsewhere is kept"), "{body:?}");
    h.tx.send(Request::Answer { id: *id, choice: Some(1) }).await.unwrap(); // Keep
    h.tx.send(Request::ListModels).await.unwrap();
    let ev = until(&mut h.rx, |e| matches!(e, Event::Models(_))).await;
    let Some(Event::Models(list)) = ev.last() else { unreachable!() };
    assert!(list.iter().any(|x| x.id == m.id), "kept after declining");
    assert!(gguf.exists());
}
