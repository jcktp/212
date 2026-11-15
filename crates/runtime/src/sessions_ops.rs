//! Session persistence, resume, history search, export and compaction.

use common::proto::{AgentMode, Event, NoticeLevel, SessionItem, TranscriptItem};
use common::{Message, Role};
use futures::StreamExt;
use providers::{ChatRequest, Sampling};
use sessions::{SessionMeta, SessionStore};

use crate::{Internal, PendingAction, Runtime};

const COMPACT_PROMPT: &str = "Summarize the conversation so far so it can be continued without the full transcript. \
Keep, verbatim where possible: the user's instructions, preferences and constraints; decisions made; file paths, commands, \
names and numbers that matter; what was done and its results; open questions and next steps. \
Use terse bullet points under the headings Instructions, Done, State, Next. At most 350 words. Do not add commentary.";

fn ago(ts: i64) -> String {
    let d = (sessions::now() - ts).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86_399 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86_400),
    }
}

pub(crate) fn transcript(messages: &[Message]) -> Vec<TranscriptItem> {
    let mut out = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        match m.role {
            Role::User => out.push(TranscriptItem::User(m.content.split("\n\nAttached files").next().unwrap_or("").to_string())),
            Role::Assistant => {
                if !m.content.trim().is_empty() || m.reasoning.is_some() {
                    out.push(TranscriptItem::Assistant { text: m.content.clone(), reasoning: m.reasoning.clone() });
                }
                for c in &m.tool_calls {
                    let result = messages[i + 1..].iter().find(|r| r.role == Role::Tool && r.tool_call_id.as_deref() == Some(&c.id));
                    let ok = result.is_some_and(|r| !r.content.starts_with("Error"));
                    let summary = result.map(|r| r.content.lines().next().unwrap_or("").chars().take(70).collect()).unwrap_or_default();
                    let args: String = c.arguments.chars().take(60).collect();
                    out.push(TranscriptItem::Tool { title: format!("{} {args}", c.name), ok, summary });
                }
            }
            Role::Tool | Role::System => {}
        }
    }
    out
}

impl Runtime {
    pub(crate) fn project_key(&self) -> String {
        std::fs::canonicalize(&self.cwd).unwrap_or_else(|_| self.cwd.clone()).display().to_string()
    }

    fn store(&self) -> Option<&SessionStore> {
        self.sessions.as_ref()
    }

    /// Persist the messages of the turn that just finished.
    pub(crate) async fn persist_turn(&mut self, from: usize) {
        if self.sessions.is_none() || from >= self.messages.len() {
            return;
        }
        if self.session.is_none() {
            let (provider, model, mode, project) = (
                self.provider_name.clone(),
                self.model_name.clone().unwrap_or_default(),
                self.mode.as_str().to_string(),
                self.project_key(),
            );
            match self.store().unwrap().create(&project, &provider, &model, &mode) {
                Ok(m) => self.session = Some(m),
                Err(e) => return self.session_error(e).await,
            }
        }
        let id = self.session.as_ref().unwrap().id.clone();
        let msgs = self.messages[from..].to_vec();
        if let Err(e) = self.store().unwrap().append(&id, &msgs) {
            self.session_error(e).await;
        }
    }

    async fn session_error(&mut self, e: sessions::SessionError) {
        tracing::error!("sessions: {e}");
        self.notice(NoticeLevel::Warn, format!("Could not save the conversation: {e}")).await;
    }

    pub(crate) fn sync_session_settings(&self) {
        if let (Some(st), Some(s)) = (self.store(), &self.session) {
            let _ = st.set_settings(&s.id, &self.provider_name, self.model_name.as_deref().unwrap_or(""), self.mode.as_str());
        }
    }

    pub(crate) async fn list_sessions(&mut self, all: bool) {
        let Some(st) = self.store() else {
            return self.notice(NoticeLevel::Info, "Sessions are disabled (sessions.autosave = false).").await;
        };
        let project = self.project_key();
        let list = match st.list(if all { None } else { Some(&project) }, 100) {
            Ok(l) => l,
            Err(e) => return self.notice(NoticeLevel::Error, e.to_string()).await,
        };
        if list.is_empty() {
            let text =
                if all { "No saved sessions yet." } else { "No saved sessions for this project yet. (/sessions all lists every project.)" };
            return self.notice(NoticeLevel::Info, text).await;
        }
        let current = self.session.as_ref().map(|s| s.id.clone());
        let items = list
            .into_iter()
            .map(|m| SessionItem {
                current: current.as_deref() == Some(&m.id),
                detail: format!(
                    "{} · {} messages · {}{}{}",
                    ago(m.updated_at),
                    m.message_count,
                    if m.model.is_empty() { m.provider.clone() } else { m.model.clone() },
                    if m.summary.is_some() { " · compacted" } else { "" },
                    if all { format!(" · {}", m.project) } else { String::new() }
                ),
                title: if m.title.is_empty() { "untitled".into() } else { m.title.clone() },
                id: m.id,
            })
            .collect();
        let title = if all { "All sessions".to_string() } else { "Sessions · this project".to_string() };
        self.emit(Event::SessionList { title, items }).await;
    }

    pub(crate) async fn resume_session(&mut self, query: Option<String>) {
        let Some(st) = self.store() else {
            return self.notice(NoticeLevel::Info, "Sessions are disabled (sessions.autosave = false).").await;
        };
        let meta = match &query {
            Some(q) => st.find(q.trim()),
            None => match st.latest(&self.project_key()) {
                Ok(Some(m)) => Ok(m),
                Ok(None) => return self.notice(NoticeLevel::Info, "No previous session in this project.").await,
                Err(e) => Err(e),
            },
        };
        let meta = match meta {
            Ok(m) => m,
            Err(e) => return self.notice(NoticeLevel::Error, e.to_string()).await,
        };
        let stored = match st.messages(&meta.id) {
            Ok(m) => m,
            Err(e) => return self.notice(NoticeLevel::Error, e.to_string()).await,
        };
        self.dismiss_permissions().await;
        if let Some(c) = self.generation.take() {
            c.cancel();
        }
        let all: Vec<Message> = stored.iter().map(|m| m.message.clone()).collect();
        self.messages = stored.iter().filter(|m| m.seq > meta.compacted_upto).map(|m| m.message.clone()).collect();
        self.summary = meta.summary.clone();
        self.grants.clear();
        self.context_used = 0;
        if let Some(mode) = AgentMode::parse(&meta.mode) {
            self.mode = mode;
        }
        let mut items = Vec::new();
        if let Some(s) = &meta.summary {
            items.push(TranscriptItem::Summary(s.clone()));
        }
        items.extend(transcript(&all));
        let title = format!("{} · {}", if meta.title.is_empty() { "untitled" } else { &meta.title }, ago(meta.updated_at));
        let loading_same = self.loading.as_ref().is_some_and(|(id, _)| *id == meta.model);
        let model_note = (!meta.model.is_empty() && !loading_same && self.model_name.as_deref() != Some(meta.model.as_str())).then(|| {
            format!(
                "This session used {}; the current model is {}. Switch with /models if you like.",
                meta.model,
                self.model_name.clone().unwrap_or_else(|| "none".into())
            )
        });
        self.session = Some(meta);
        self.emit(Event::Transcript { title, items }).await;
        if let Some(n) = model_note {
            self.notice(NoticeLevel::Info, n).await;
        }
        self.snapshot().await;
    }

    pub(crate) async fn rename_session(&mut self, title: &str) {
        let title = title.trim();
        if title.is_empty() {
            return self.notice(NoticeLevel::Info, "Usage: /rename <new title>").await;
        }
        let (Some(st), Some(s)) = (self.sessions.as_ref(), self.session.as_mut()) else {
            return self.notice(NoticeLevel::Info, "Nothing to rename yet — the session is saved after the first reply.").await;
        };
        match st.rename(&s.id, title) {
            Ok(()) => {
                s.title = title.to_string();
                self.notice(NoticeLevel::Success, format!("Session renamed to \"{title}\".")).await;
            }
            Err(e) => self.notice(NoticeLevel::Error, e.to_string()).await,
        }
    }

    pub(crate) async fn delete_session(&mut self, query: &str) {
        let Some(st) = self.store() else { return };
        let meta = match st.find(query) {
            Ok(m) => m,
            Err(e) => return self.notice(NoticeLevel::Error, e.to_string()).await,
        };
        let title = if meta.title.is_empty() { "untitled".to_string() } else { meta.title.clone() };
        self.choice(
            PendingAction::DeleteSession(meta.id.clone()),
            format!("Delete \"{title}\"?"),
            vec![format!("{} messages will be permanently deleted. This cannot be undone.", meta.message_count)],
            vec!["Delete".into(), "Keep".into()],
        )
        .await;
    }

    pub(crate) async fn delete_session_confirmed(&mut self, id: &str) {
        let Some(st) = self.store() else { return };
        match st.delete(id) {
            Ok(()) => {
                if self.session.as_ref().is_some_and(|s| s.id == id) {
                    self.session = None;
                }
                self.notice(NoticeLevel::Success, "Session deleted.").await;
            }
            Err(e) => self.notice(NoticeLevel::Error, e.to_string()).await,
        }
    }

    pub(crate) async fn search_history(&mut self, query: &str) {
        let Some(st) = self.store() else {
            return self.notice(NoticeLevel::Info, "Sessions are disabled (sessions.autosave = false).").await;
        };
        if query.trim().is_empty() {
            return self.notice(NoticeLevel::Info, "Usage: /history <words to search for>").await;
        }
        let here = self.project_key();
        match st.search(query, None, 30) {
            Ok(hits) if hits.is_empty() => self.notice(NoticeLevel::Info, format!("No saved messages match \"{query}\".")).await,
            Ok(hits) => {
                let items = hits
                    .into_iter()
                    .map(|h| SessionItem {
                        current: self.session.as_ref().is_some_and(|s| s.id == h.session_id),
                        detail: format!(
                            "{} · {}{}",
                            ago(h.updated_at),
                            h.snippet.replace('\n', " "),
                            if h.project != here { format!(" · {}", h.project) } else { String::new() }
                        ),
                        title: if h.title.is_empty() { "untitled".into() } else { h.title },
                        id: h.session_id,
                    })
                    .collect();
                self.emit(Event::SessionList { title: format!("History · {query}"), items }).await;
            }
            Err(e) => self.notice(NoticeLevel::Error, e.to_string()).await,
        }
    }

    pub(crate) async fn export(&mut self, format: &str, path: Option<String>) {
        let format = match format.trim().to_lowercase().as_str() {
            "" | "md" | "markdown" => "md",
            "json" => "json",
            other => return self.notice(NoticeLevel::Error, format!("Unknown export format '{other}'. Use md or json.")).await,
        };
        let (meta, stored) = match (self.store(), &self.session) {
            (Some(st), Some(s)) => match (st.get(&s.id), st.messages(&s.id)) {
                (Ok(m), Ok(msgs)) => (m, msgs),
                (Err(e), _) | (_, Err(e)) => return self.notice(NoticeLevel::Error, e.to_string()).await,
            },
            _ if !self.messages.is_empty() => {
                // Not persisted (autosave off): export what's in memory.
                let meta = SessionMeta {
                    id: "unsaved".into(),
                    title: sessions::auto_title(self.messages.iter().find(|m| m.role == Role::User).map_or("", |m| m.content.as_str())),
                    project: self.project_key(),
                    created_at: sessions::now(),
                    updated_at: sessions::now(),
                    provider: self.provider_name.clone(),
                    model: self.model_name.clone().unwrap_or_default(),
                    mode: self.mode.as_str().into(),
                    message_count: self.messages.len() as i64,
                    summary: self.summary.clone(),
                    compacted_upto: 0,
                };
                let msgs = self
                    .messages
                    .iter()
                    .enumerate()
                    .map(|(i, m)| sessions::StoredMessage { seq: i as i64 + 1, message: m.clone(), created_at: 0 })
                    .collect();
                (meta, msgs)
            }
            _ => return self.notice(NoticeLevel::Info, "Nothing to export yet.").await,
        };
        let body = if format == "md" { sessions::to_markdown(&meta, &stored) } else { sessions::to_json(&meta, &stored) };
        let target = match path.filter(|p| !p.trim().is_empty()) {
            Some(p) => {
                let p = std::path::PathBuf::from(p.trim());
                if p.is_absolute() { p } else { self.cwd.join(p) }
            }
            None => {
                let slug: String = models::store::slug(&meta.title).chars().take(40).collect();
                let base = self.cwd.join(format!("212-{slug}-{}.{format}", &meta.id[..meta.id.len().min(6)]));
                let mut p = base.clone();
                let mut n = 2;
                while p.exists() {
                    p = base.with_file_name(format!("{}-{n}.{format}", base.file_stem().unwrap_or_default().to_string_lossy()));
                    n += 1;
                }
                p
            }
        };
        match std::fs::write(&target, body) {
            Ok(()) => self.notice(NoticeLevel::Success, format!("Exported {} messages to {}", stored.len(), target.display())).await,
            Err(e) => self.notice(NoticeLevel::Error, format!("Could not write {}: {e}", target.display())).await,
        }
    }

    /// Index up to which history can be summarized, keeping the last two exchanges.
    fn compact_boundary(&self) -> Option<usize> {
        let users: Vec<usize> = self.messages.iter().enumerate().filter(|(_, m)| m.role == Role::User).map(|(i, _)| i).collect();
        let keep_from = *users.iter().rev().nth(1)?;
        (keep_from > 0).then_some(keep_from)
    }

    /// Start summarizing older history. `then_send` is sent once done (auto-compaction).
    pub(crate) async fn compact(&mut self, then_send: Option<String>) {
        if self.generation.is_some() {
            return self.notice(NoticeLevel::Warn, "Wait for the current reply to finish (or press Esc) before compacting.").await;
        }
        let Some(provider) = self.provider.clone() else {
            return self.notice(NoticeLevel::Warn, "No model is loaded to write the summary.").await;
        };
        let Some(upto) = self.compact_boundary() else {
            if let Some(t) = then_send {
                self.skip_auto_compact = true;
                return Box::pin(self.send(t)).await;
            }
            return self.notice(NoticeLevel::Info, "Not enough history to compact yet.").await;
        };
        let mut msgs = vec![Message::system(COMPACT_PROMPT)];
        if let Some(s) = &self.summary {
            msgs.push(Message::user(format!("Summary of the conversation before this point:\n{s}")));
            msgs.push(Message::assistant("Noted."));
        }
        // Tool results are long and mostly stale; keep their first lines only.
        for m in &self.messages[..upto] {
            let mut m = m.clone();
            if m.role == Role::Tool {
                m.content = m.content.chars().take(600).collect();
            }
            msgs.push(m);
        }
        msgs.push(Message::user("Write the summary now."));
        // Some providers reject dangling tool calls in the last position; flatten roles.
        let req = ChatRequest {
            model: self.model_name.clone().unwrap_or_default(),
            messages: flatten_for_summary(msgs),
            sampling: Sampling { temperature: Some(0.2), top_p: None, max_tokens: Some(700) },
            tools: Vec::new(),
        };
        self.notice(NoticeLevel::Info, format!("Compacting {} older messages into a summary…", upto)).await;
        self.emit(Event::Inference(common::proto::InferenceState::Generating)).await;
        let cancel = tokio_util::sync::CancellationToken::new();
        self.generation = Some(cancel.clone());
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let mut stream = provider.chat(req);
            let mut text = String::new();
            let mut err = None;
            loop {
                let n =
                    tokio::select! { n = stream.next() => n, _ = cancel.cancelled() => { err = Some("cancelled".to_string()); break; } };
                match n {
                    Some(Ok(common::StreamEvent::Text(t))) => text.push_str(&t),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        err = Some(e.to_string());
                        break;
                    }
                    None => break,
                }
            }
            let result = match err {
                Some(e) => Err(e),
                None if text.trim().is_empty() => Err("the model returned an empty summary".into()),
                None => Ok(text.trim().to_string()),
            };
            let _ = tx.send(Internal::Compacted { result, upto, then_send }).await;
        });
    }

    pub(crate) async fn on_compacted(&mut self, result: Result<String, String>, upto: usize, then_send: Option<String>) {
        self.generation = None;
        self.emit(Event::Inference(common::proto::InferenceState::Ready)).await;
        match result {
            Ok(summary) => {
                let dropped: Vec<Message> = self.messages.drain(..upto).collect();
                if let (Some(st), Some(s)) = (self.store(), &self.session) {
                    // Persisted messages = compacted_upto + in-memory history.
                    let upto_seq = s.compacted_upto + dropped.len() as i64;
                    if let Err(e) = st.set_compaction(&s.id, &summary, upto_seq) {
                        tracing::warn!("saving compaction: {e}");
                    }
                    if let Some(s) = self.session.as_mut() {
                        s.compacted_upto = upto_seq;
                        s.summary = Some(summary.clone());
                    }
                }
                self.summary = Some(summary.clone());
                self.context_used = 0;
                self.emit(Event::Transcript { title: String::new(), items: vec![TranscriptItem::Summary(summary)] }).await;
                self.notice(
                    NoticeLevel::Success,
                    format!("Compacted {} messages into a summary. The full transcript stays saved (/export).", dropped.len()),
                )
                .await;
            }
            Err(e) => self.notice(NoticeLevel::Warn, format!("Compaction failed ({e}); continuing without it.")).await,
        }
        self.snapshot().await;
        if let Some(t) = then_send {
            self.skip_auto_compact = true;
            self.send(t).await;
        }
    }
}

/// Summary requests contain only user/assistant text so any provider accepts them.
fn flatten_for_summary(msgs: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for m in msgs {
        let (role, content) = match m.role {
            Role::System => (Role::System, m.content),
            Role::User => (Role::User, m.content),
            Role::Assistant => {
                let calls: Vec<String> = m.tool_calls.iter().map(|c| format!("[called {} {}]", c.name, c.arguments)).collect();
                (Role::Assistant, format!("{}{}{}", m.content, if calls.is_empty() { "" } else { "\n" }, calls.join("\n")))
            }
            Role::Tool => (Role::User, format!("[tool result]\n{}", m.content)),
        };
        if content.trim().is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.role == role && role != Role::System => {
                last.content.push_str("\n\n");
                last.content.push_str(&content);
            }
            _ => out.push(Message::new(role, content)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::ToolCall;

    #[test]
    fn flattening_alternates_roles() {
        let mut a = Message::assistant("checking");
        a.tool_calls = vec![ToolCall { id: "1".into(), name: "ls".into(), arguments: "{}".into() }];
        let mut t = Message::new(Role::Tool, "a.txt");
        t.tool_call_id = Some("1".into());
        let out = flatten_for_summary(vec![Message::system("s"), Message::user("hi"), a, t, Message::user("next")]);
        let roles: Vec<Role> = out.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::System, Role::User, Role::Assistant, Role::User]);
        assert!(out[2].content.contains("[called ls {}]"));
        assert!(out[3].content.contains("[tool result]\na.txt") && out[3].content.ends_with("next"));
    }

    #[test]
    fn transcript_pairs_tool_calls_with_results() {
        let mut a = Message::assistant("");
        a.tool_calls = vec![ToolCall { id: "1".into(), name: "read_file".into(), arguments: r#"{"path":"x"}"#.into() }];
        let mut t = Message::new(Role::Tool, "x (3 lines):\nabc");
        t.tool_call_id = Some("1".into());
        let items = transcript(&[Message::user("q\n\nAttached files (…):\n<file>"), a, t, Message::assistant("done")]);
        assert_eq!(items[0], TranscriptItem::User("q".into()));
        assert!(matches!(&items[1], TranscriptItem::Tool { ok: true, summary, .. } if summary == "x (3 lines):"));
        assert!(matches!(&items[2], TranscriptItem::Assistant { text, .. } if text == "done"));
    }
}
