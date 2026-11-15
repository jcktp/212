//! Conversation persistence.
//!
//! One SQLite database (WAL mode) holds all sessions. Messages are appended
//! as they happen, full-text indexed (FTS5) for `/history`, and grouped by
//! project (the canonical working directory). Compaction never deletes
//! messages: it records a summary and the sequence number it covers, so the
//! full transcript stays available for export and search.
//!
//! Nothing secret is written here: API keys live in the OS keychain or the
//! environment, never in messages.

use std::path::Path;

use common::{Message, Role, ToolCall};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("no session matches '{0}'")]
    NotFound(String),
    #[error("'{0}' matches several sessions; use more of the id")]
    Ambiguous(String),
    #[error("session database was created by a newer 212 (schema {0})")]
    TooNew(i64),
}

pub type Result<T> = std::result::Result<T, SessionError>;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub project: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub provider: String,
    pub model: String,
    pub mode: String,
    pub message_count: i64,
    /// Summary from the last compaction, if any.
    pub summary: Option<String>,
    /// Messages with `seq <= compacted_upto` are covered by `summary`.
    pub compacted_upto: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessage {
    pub seq: i64,
    pub message: Message,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub session_id: String,
    pub title: String,
    pub project: String,
    pub updated_at: i64,
    pub snippet: String,
}

/// Thread-safe handle (the connection sits behind a mutex; calls are short).
pub struct SessionStore {
    conn: std::sync::Mutex<Connection>,
}

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn new_id() -> String {
    use rand::Rng;
    let mut r = rand::rng();
    let t = now() as u64;
    // Random first so short prefixes are unique; time suffix keeps ids sortable-ish.
    format!("{:06x}{:08x}", r.random_range(0..0x100_0000u32), t & 0xffff_ffff)
}

fn role_str(r: Role) -> &'static str {
    r.as_str()
}

fn parse_role(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

impl SessionStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(SessionError::TooNew(version));
        }
        if version < 1 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    project TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    provider TEXT NOT NULL DEFAULT '',
                    model TEXT NOT NULL DEFAULT '',
                    mode TEXT NOT NULL DEFAULT 'chat',
                    summary TEXT,
                    compacted_upto INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX IF NOT EXISTS sessions_project ON sessions(project, updated_at);
                 CREATE TABLE IF NOT EXISTS messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                    seq INTEGER NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL,
                    reasoning TEXT,
                    tool_calls TEXT,
                    tool_call_id TEXT,
                    created_at INTEGER NOT NULL,
                    UNIQUE(session_id, seq)
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(content, content='messages', content_rowid='id');
                 CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
                 END;
                 CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
                 END;
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        // Future migrations: `if version < 2 { … PRAGMA user_version = 2; }`.
        Ok(Self { conn: std::sync::Mutex::new(conn) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn create(&self, project: &str, provider: &str, model: &str, mode: &str) -> Result<SessionMeta> {
        let id = new_id();
        let t = now();
        self.db().execute(
            "INSERT INTO sessions (id, title, project, created_at, updated_at, provider, model, mode) VALUES (?1, '', ?2, ?3, ?3, ?4, ?5, ?6)",
            params![id, project, t, provider, model, mode],
        )?;
        self.get(&id)
    }

    fn row_meta(r: &rusqlite::Row) -> rusqlite::Result<SessionMeta> {
        Ok(SessionMeta {
            id: r.get(0)?,
            title: r.get(1)?,
            project: r.get(2)?,
            created_at: r.get(3)?,
            updated_at: r.get(4)?,
            provider: r.get(5)?,
            model: r.get(6)?,
            mode: r.get(7)?,
            summary: r.get(8)?,
            compacted_upto: r.get(9)?,
            message_count: r.get(10)?,
        })
    }

    const META_SQL: &'static str =
        "SELECT s.id, s.title, s.project, s.created_at, s.updated_at, s.provider, s.model, s.mode, s.summary, s.compacted_upto,
            (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) FROM sessions s";

    pub fn get(&self, id: &str) -> Result<SessionMeta> {
        self.db()
            .query_row(&format!("{} WHERE s.id = ?1", Self::META_SQL), params![id], Self::row_meta)
            .optional()?
            .ok_or_else(|| SessionError::NotFound(id.into()))
    }

    /// Exact id, else a unique id prefix.
    pub fn find(&self, query: &str) -> Result<SessionMeta> {
        if let Ok(m) = self.get(query) {
            return Ok(m);
        }
        let ids: Vec<String> = {
            let db = self.db();
            let mut st = db.prepare("SELECT id FROM sessions WHERE id LIKE ?1 || '%' LIMIT 2")?;
            st.query_map(params![query], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        match ids.as_slice() {
            [one] => self.get(one),
            [] => Err(SessionError::NotFound(query.into())),
            _ => Err(SessionError::Ambiguous(query.into())),
        }
    }

    /// Sessions, most recent first. `project = None` lists all projects.
    pub fn list(&self, project: Option<&str>, limit: usize) -> Result<Vec<SessionMeta>> {
        let sql = format!(
            "{} WHERE (?1 IS NULL OR s.project = ?1) AND EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id) ORDER BY s.updated_at DESC, s.rowid DESC LIMIT ?2",
            Self::META_SQL
        );
        let db = self.db();
        let mut st = db.prepare(&sql)?;
        let rows = st.query_map(params![project, limit as i64], Self::row_meta)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The most recently updated non-empty session in a project.
    pub fn latest(&self, project: &str) -> Result<Option<SessionMeta>> {
        Ok(self.list(Some(project), 1)?.into_iter().next())
    }

    /// Append messages. The first user message also becomes the title if none is set.
    pub fn append(&self, id: &str, messages: &[Message]) -> Result<()> {
        let db = self.db();
        let tx = db.unchecked_transaction()?;
        let mut seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1", params![id], |r| r.get(0))?;
        let t = now();
        for m in messages {
            seq += 1;
            let calls = (!m.tool_calls.is_empty()).then(|| serde_json::to_string(&m.tool_calls).unwrap_or_default());
            tx.execute(
                "INSERT INTO messages (session_id, seq, role, content, reasoning, tool_calls, tool_call_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, seq, role_str(m.role), m.content, m.reasoning, calls, m.tool_call_id, t],
            )?;
        }
        if let Some(first) = messages.iter().find(|m| m.role == Role::User) {
            let title = auto_title(&first.content);
            tx.execute("UPDATE sessions SET title = ?2 WHERE id = ?1 AND title = ''", params![id, title])?;
        }
        tx.execute("UPDATE sessions SET updated_at = ?2 WHERE id = ?1", params![id, t])?;
        tx.commit()?;
        Ok(())
    }

    pub fn messages(&self, id: &str) -> Result<Vec<StoredMessage>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT seq, role, content, reasoning, tool_calls, tool_call_id, created_at FROM messages WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = st.query_map(params![id], |r| {
            let calls: Option<String> = r.get(4)?;
            let tool_calls: Vec<ToolCall> = calls.and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default();
            Ok(StoredMessage {
                seq: r.get(0)?,
                message: Message {
                    role: parse_role(&r.get::<_, String>(1)?),
                    content: r.get(2)?,
                    reasoning: r.get(3)?,
                    tool_calls,
                    tool_call_id: r.get(5)?,
                },
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<()> {
        let n = self.db().execute("UPDATE sessions SET title = ?2 WHERE id = ?1", params![id, title.trim()])?;
        if n == 0 { Err(SessionError::NotFound(id.into())) } else { Ok(()) }
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let n = self.db().execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        if n == 0 { Err(SessionError::NotFound(id.into())) } else { Ok(()) }
    }

    pub fn set_settings(&self, id: &str, provider: &str, model: &str, mode: &str) -> Result<()> {
        self.db().execute("UPDATE sessions SET provider = ?2, model = ?3, mode = ?4 WHERE id = ?1", params![id, provider, model, mode])?;
        Ok(())
    }

    /// Record a compaction: `summary` replaces messages up to `upto` in context.
    pub fn set_compaction(&self, id: &str, summary: &str, upto: i64) -> Result<()> {
        self.db().execute(
            "UPDATE sessions SET summary = ?2, compacted_upto = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, summary, upto, now()],
        )?;
        Ok(())
    }

    pub fn max_seq(&self, id: &str) -> Result<i64> {
        Ok(self.db().query_row("SELECT COALESCE(MAX(seq), 0) FROM messages WHERE session_id = ?1", params![id], |r| r.get(0))?)
    }

    /// Full-text search over user and assistant messages.
    pub fn search(&self, query: &str, project: Option<&str>, limit: usize) -> Result<Vec<Hit>> {
        let fts = fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let db = self.db();
        let mut st = db.prepare(
            "SELECT s.id, s.title, s.project, s.updated_at, snippet(messages_fts, 0, '«', '»', '…', 12)
             FROM messages_fts JOIN messages m ON m.id = messages_fts.rowid JOIN sessions s ON s.id = m.session_id
             WHERE messages_fts MATCH ?1 AND m.role IN ('user', 'assistant') AND (?2 IS NULL OR s.project = ?2)
             ORDER BY rank LIMIT ?3",
        )?;
        let rows = st.query_map(params![fts, project, (limit * 4) as i64], |r| {
            Ok(Hit { session_id: r.get(0)?, title: r.get(1)?, project: r.get(2)?, updated_at: r.get(3)?, snippet: r.get(4)? })
        })?;
        // One hit per session, best first.
        let mut out: Vec<Hit> = Vec::new();
        for h in rows {
            let h = h?;
            if !out.iter().any(|o| o.session_id == h.session_id) {
                out.push(h);
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}

/// Turn free text into a safe FTS5 query: every word is a quoted prefix term.
pub fn fts_query(q: &str) -> String {
    q.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect::<String>())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn auto_title(text: &str) -> String {
    // Skip attached file blocks; use the first line of what the user typed.
    let head = text.split("\n\nAttached files").next().unwrap_or(text);
    let line = head.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        t.push('…');
    }
    if t.is_empty() { "untitled".into() } else { t }
}

fn date(ts: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(ts)
        .ok()
        .and_then(|d| d.format(&time::macros::format_description!("[year]-[month]-[day] [hour]:[minute] UTC")).ok())
        .unwrap_or_default()
}

/// Human-readable Markdown transcript.
pub fn to_markdown(meta: &SessionMeta, messages: &[StoredMessage]) -> String {
    let mut s = format!(
        "# {}\n\n- Session: `{}`\n- Project: `{}`\n- Created: {}\n- Model: {} ({})\n- Mode: {}\n\n",
        if meta.title.is_empty() { "Untitled session" } else { &meta.title },
        meta.id,
        meta.project,
        date(meta.created_at),
        if meta.model.is_empty() { "unknown" } else { &meta.model },
        meta.provider,
        meta.mode
    );
    if let Some(sum) = &meta.summary {
        s.push_str(&format!("> **Compacted summary** (covers messages 1–{}):\n>\n", meta.compacted_upto));
        for l in sum.lines() {
            s.push_str(&format!("> {l}\n"));
        }
        s.push('\n');
    }
    for m in messages {
        let msg = &m.message;
        match msg.role {
            Role::User => s.push_str(&format!("## You\n\n{}\n\n", msg.content.trim())),
            Role::Assistant => {
                s.push_str("## 212\n\n");
                if let Some(r) = &msg.reasoning {
                    s.push_str(&format!("<details><summary>Reasoning</summary>\n\n{}\n\n</details>\n\n", r.trim()));
                }
                if !msg.content.trim().is_empty() {
                    s.push_str(&format!("{}\n\n", msg.content.trim()));
                }
                for c in &msg.tool_calls {
                    s.push_str(&format!("- 🔧 `{}` `{}`\n", c.name, c.arguments.chars().take(200).collect::<String>()));
                }
                if !msg.tool_calls.is_empty() {
                    s.push('\n');
                }
            }
            Role::Tool => {
                let body: String = msg.content.chars().take(2000).collect();
                let fence = if body.contains("```") { "~~~" } else { "```" };
                s.push_str(&format!("<details><summary>Tool result</summary>\n\n{fence}\n{body}\n{fence}\n\n</details>\n\n"));
            }
            Role::System => {}
        }
    }
    s
}

#[derive(Serialize)]
struct JsonExport<'a> {
    format: &'static str,
    session: &'a SessionMeta,
    messages: Vec<&'a Message>,
}

pub fn to_json(meta: &SessionMeta, messages: &[StoredMessage]) -> String {
    let e = JsonExport { format: "212-session-v1", session: meta, messages: messages.iter().map(|m| &m.message).collect() };
    serde_json::to_string_pretty(&e).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_msg(id: &str, content: &str) -> Message {
        let mut m = Message::new(Role::Tool, content);
        m.tool_call_id = Some(id.into());
        m
    }

    #[test]
    fn roundtrip_list_rename_delete() {
        let st = SessionStore::in_memory().unwrap();
        let a = st.create("/p/one", "local", "qwen", "build").unwrap();
        let empty = st.create("/p/one", "local", "qwen", "chat").unwrap();
        let b = st.create("/p/two", "anthropic", "claude", "chat").unwrap();
        let mut asst = Message::assistant("Let me look.");
        asst.tool_calls = vec![ToolCall { id: "c1".into(), name: "read_file".into(), arguments: r#"{"path":"a"}"#.into() }];
        st.append(
            &a.id,
            &[Message::user("Fix the parser bug\nplease"), asst, tool_msg("c1", "fn parse() {}"), Message::assistant("Fixed.")],
        )
        .unwrap();
        st.append(&b.id, &[Message::user("hello there")]).unwrap();

        let loaded = st.messages(&a.id).unwrap();
        assert_eq!(loaded.len(), 4);
        assert_eq!(loaded[1].message.tool_calls[0].name, "read_file");
        assert_eq!(loaded[2].message.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(loaded.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4]);

        let one = st.list(Some("/p/one"), 10).unwrap();
        assert_eq!(one.len(), 1, "empty sessions are not listed");
        assert_eq!(one[0].title, "Fix the parser bug");
        assert_eq!(one[0].message_count, 4);
        assert_eq!(st.list(None, 10).unwrap().len(), 2);
        assert_eq!(st.latest("/p/two").unwrap().unwrap().id, b.id);

        st.rename(&a.id, "Parser fix").unwrap();
        assert_eq!(st.find(&a.id[..6]).unwrap().title, "Parser fix");
        st.delete(&a.id).unwrap();
        assert!(st.messages(&a.id).unwrap().is_empty(), "messages cascade");
        assert!(matches!(st.find(&a.id), Err(SessionError::NotFound(_))));
        let _ = empty;
    }

    #[test]
    fn full_text_search() {
        let st = SessionStore::in_memory().unwrap();
        let a = st.create("/p", "local", "m", "chat").unwrap();
        st.append(&a.id, &[Message::user("How do I configure the tokenizer?"), Message::assistant("Set tokenizer.mode = fast")]).unwrap();
        let b = st.create("/q", "local", "m", "chat").unwrap();
        st.append(&b.id, &[Message::user("unrelated question about pancakes")]).unwrap();
        let hits = st.search("tokeniz", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, a.id);
        assert!(hits[0].snippet.contains('«'));
        assert!(st.search("pancakes", Some("/p"), 10).unwrap().is_empty());
        // Hostile syntax is neutralized.
        assert!(st.search("\") OR * NEAR(", None, 10).is_ok());
        assert!(st.search("   ", None, 10).unwrap().is_empty());
        // Deleted sessions vanish from the index.
        st.delete(&a.id).unwrap();
        assert!(st.search("tokenizer", None, 10).unwrap().is_empty());
    }

    #[test]
    fn compaction_and_export() {
        let st = SessionStore::in_memory().unwrap();
        let a = st.create("/p", "local", "m", "chat").unwrap();
        st.append(&a.id, &[Message::user("first"), Message::assistant("one"), Message::user("second"), Message::assistant("two")]).unwrap();
        st.set_compaction(&a.id, "User asked twice.", 2).unwrap();
        let meta = st.get(&a.id).unwrap();
        assert_eq!(meta.compacted_upto, 2);
        let msgs = st.messages(&a.id).unwrap();
        let md = to_markdown(&meta, &msgs);
        assert!(md.starts_with("# first"));
        assert!(md.contains("Compacted summary") && md.contains("## You\n\nsecond"));
        let json: serde_json::Value = serde_json::from_str(&to_json(&meta, &msgs)).unwrap();
        assert_eq!(json["format"], "212-session-v1");
        assert_eq!(json["messages"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn persists_on_disk_and_rejects_newer_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s.sqlite3");
        let id = {
            let st = SessionStore::open(&p).unwrap();
            let s = st.create("/p", "local", "m", "chat").unwrap();
            st.append(&s.id, &[Message::user("persist me")]).unwrap();
            s.id
        };
        let st = SessionStore::open(&p).unwrap();
        assert_eq!(st.messages(&id).unwrap()[0].message.content, "persist me");
        drop(st);
        let c = Connection::open(&p).unwrap();
        c.pragma_update(None, "user_version", 99).unwrap();
        drop(c);
        assert!(matches!(SessionStore::open(&p), Err(SessionError::TooNew(99))));
    }

    #[test]
    fn titles_skip_attachments() {
        assert_eq!(auto_title("explain this\n\nAttached files (contents…):\n<file>"), "explain this");
        assert_eq!(auto_title(&"x".repeat(80)).chars().count(), 61);
        assert_eq!(auto_title("  "), "untitled");
    }
}
