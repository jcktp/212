//! Agent tools.
//!
//! Every tool call goes through two steps:
//!
//! 1. [`plan`] parses and validates the model's JSON arguments, resolves paths
//!    inside the workspace and, for mutating tools, computes exactly what will
//!    happen (a unified diff, the command line). Nothing is changed yet.
//! 2. [`execute`] performs the planned action. The runtime calls this only
//!    after the permission policy (and, if needed, the user) approved the plan.
//!
//! Tools never touch anything outside the workspace root: paths are resolved
//! lexically and through symlinks, and must stay under the root. Tool output is
//! data for the model, never instructions; the runtime labels it as such.

mod exec;
mod fsops;
mod paths;
mod project;
pub mod web;

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub use exec::danger;
pub use paths::{Workspace, is_sensitive};
pub use project::brief as project_brief;

/// What a tool can do, which determines its default permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Reads inside the workspace. Allowed in plan and build modes without asking.
    Read,
    /// Creates or modifies files. Build mode only; needs approval.
    Write,
    /// Runs a process. Build mode only; needs approval.
    Execute,
    /// Reads a web page. Any mode; asks per site unless configured otherwise.
    Network,
    /// Loads an installed skill's instructions. Any mode; never asks.
    Skill,
}

/// An installed skill the model may load.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillRef {
    pub name: String,
    pub dir: PathBuf,
}

/// Shared execution context.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    pub workspace: Workspace,
    pub cancel: CancellationToken,
    pub command_timeout: Duration,
    /// Environment variables removed before running commands (API keys etc.).
    pub scrub_env: Vec<String>,
    pub skills: Vec<SkillRef>,
    pub http: Option<web::Client>,
}

/// A validated, ready-to-run tool call.
#[derive(Debug, Clone)]
pub struct Plan {
    pub tool: String,
    pub kind: ToolKind,
    /// One-line description for the UI, e.g. `edit src/main.rs`.
    pub title: String,
    /// What the user approves: a unified diff or a command line.
    pub preview: Option<String>,
    /// Why this action is considered risky, if it is.
    pub danger: Option<String>,
    /// Key used for "allow for this session" grants.
    pub grant_key: String,
    action: Action,
}

#[derive(Debug, Clone)]
enum Action {
    Read { path: PathBuf, rel: String, offset: usize, limit: usize },
    List { path: PathBuf, rel: String, depth: usize },
    Find { pattern: String },
    Grep { pattern: String, path: PathBuf, glob: Option<String>, case_insensitive: bool },
    Project,
    Git { args: Vec<String> },
    Write { path: PathBuf, rel: String, content: String, original: Option<String> },
    Edit { path: PathBuf, rel: String, original: String, updated: String },
    Command { command: String, timeout: Duration },
    Fetch { url: String },
    LoadSkill { skill: SkillRef },
}

/// Result of running a tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub ok: bool,
    /// Text returned to the model.
    pub content: String,
    /// Short result for the activity log, e.g. `120 lines`, `exit 0`.
    pub summary: String,
}

impl ToolOutput {
    fn ok(content: String, summary: String) -> Self {
        Self { ok: true, content, summary }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        let msg = msg.into();
        Self { ok: false, summary: msg.lines().next().unwrap_or("error").chars().take(80).collect(), content: format!("Error: {msg}") }
    }
}

/// Upper bound on what a single tool result may contribute to the context.
pub const MAX_RESULT_CHARS: usize = 16_000;

struct Spec {
    name: &'static str,
    kind: ToolKind,
    description: &'static str,
    params: fn() -> Value,
}

const SPECS: &[Spec] = &[
    Spec {
        name: "read_file",
        kind: ToolKind::Read,
        description: "Read a text file in the workspace. Returns the requested lines.",
        params: || {
            json!({"type":"object","properties":{
                "path":{"type":"string","description":"Path relative to the workspace root"},
                "offset":{"type":"integer","description":"First line to read (1-based, default 1)"},
                "limit":{"type":"integer","description":"Maximum number of lines (default 400)"}},
              "required":["path"]})
        },
    },
    Spec {
        name: "list_dir",
        kind: ToolKind::Read,
        description: "List files and directories (respects .gitignore).",
        params: || {
            json!({"type":"object","properties":{
                "path":{"type":"string","description":"Directory relative to the workspace root (default '.')"},
                "depth":{"type":"integer","description":"How many levels deep (1-4, default 2)"}}})
        },
    },
    Spec {
        name: "find_files",
        kind: ToolKind::Read,
        description: "Find files by name. Accepts a glob like 'src/**/*.rs' or a plain substring of the path.",
        params: || json!({"type":"object","properties":{"pattern":{"type":"string"}},"required":["pattern"]}),
    },
    Spec {
        name: "grep",
        kind: ToolKind::Read,
        description: "Search file contents with a regular expression. Returns matching lines as path:line: text.",
        params: || {
            json!({"type":"object","properties":{
                "pattern":{"type":"string","description":"Regular expression"},
                "path":{"type":"string","description":"Directory or file to search (default '.')"},
                "glob":{"type":"string","description":"Only search files matching this glob, e.g. '*.py'"},
                "case_insensitive":{"type":"boolean"}},
              "required":["pattern"]})
        },
    },
    Spec {
        name: "project_info",
        kind: ToolKind::Read,
        description: "Summarize the project: languages, manifests, frameworks, git state and the likely test command.",
        params: || json!({"type":"object","properties":{}}),
    },
    Spec {
        name: "git_status",
        kind: ToolKind::Read,
        description: "Show the git branch and changed files.",
        params: || json!({"type":"object","properties":{}}),
    },
    Spec {
        name: "git_diff",
        kind: ToolKind::Read,
        description: "Show uncommitted changes as a diff.",
        params: || {
            json!({"type":"object","properties":{
                "path":{"type":"string","description":"Limit to this path"},
                "staged":{"type":"boolean","description":"Show staged instead of unstaged changes"}}})
        },
    },
    Spec {
        name: "git_log",
        kind: ToolKind::Read,
        description: "Show recent commits.",
        params: || json!({"type":"object","properties":{"count":{"type":"integer","description":"Number of commits (default 10, max 50)"}}}),
    },
    Spec {
        name: "web_fetch",
        kind: ToolKind::Network,
        description: "Fetch a web page or text document (http/https) and return its readable text. Use for documentation and links the user mentions.",
        params: || json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}),
    },
    Spec {
        name: "load_skill",
        kind: ToolKind::Skill,
        description: "Load the full instructions of an installed skill by name. Do this before doing a task the skill covers.",
        params: || json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}),
    },
    Spec {
        name: "write_file",
        kind: ToolKind::Write,
        description: "Create a new file or replace a file's entire content. Prefer edit_file for small changes.",
        params: || {
            json!({"type":"object","properties":{
                "path":{"type":"string"},
                "content":{"type":"string","description":"The complete new file content"}},
              "required":["path","content"]})
        },
    },
    Spec {
        name: "edit_file",
        kind: ToolKind::Write,
        description: "Replace an exact snippet in a file. old_string must match the file exactly (including whitespace) and be unique unless replace_all is true.",
        params: || {
            json!({"type":"object","properties":{
                "path":{"type":"string"},
                "old_string":{"type":"string"},
                "new_string":{"type":"string"},
                "replace_all":{"type":"boolean"}},
              "required":["path","old_string","new_string"]})
        },
    },
    Spec {
        name: "run_command",
        kind: ToolKind::Execute,
        description: "Run a shell command in the workspace root and return its exit code and output. Use for builds, linters and other project tasks.",
        params: || {
            json!({"type":"object","properties":{
                "command":{"type":"string"},
                "timeout_secs":{"type":"integer","description":"Default 120, max 600"}},
              "required":["command"]})
        },
    },
    Spec {
        name: "run_tests",
        kind: ToolKind::Execute,
        description: "Run the project's test suite (detected automatically, e.g. cargo test, npm test, pytest, go test).",
        params: || json!({"type":"object","properties":{"filter":{"type":"string","description":"Optional test name filter"}}}),
    },
];

pub fn kind_of(name: &str) -> Option<ToolKind> {
    SPECS.iter().find(|s| s.name == name).map(|s| s.kind)
}

/// OpenAI-format function definitions for the tools of the allowed kinds.
pub fn definitions(allowed: &[ToolKind]) -> Vec<Value> {
    SPECS
        .iter()
        .filter(|s| allowed.contains(&s.kind))
        .map(|s| json!({"type":"function","function":{"name":s.name,"description":s.description,"parameters":(s.params)()}}))
        .collect()
}

fn args<T: for<'de> Deserialize<'de>>(tool: &str, raw: &str) -> Result<T, String> {
    let raw = raw.trim();
    let raw = if raw.is_empty() { "{}" } else { raw };
    serde_json::from_str(raw).map_err(|e| format!("invalid arguments for {tool}: {e}"))
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}
#[derive(Deserialize)]
struct ListArgs {
    path: Option<String>,
    depth: Option<usize>,
}
#[derive(Deserialize)]
struct FindArgs {
    pattern: String,
}
#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    case_insensitive: Option<bool>,
}
#[derive(Deserialize)]
struct DiffArgs {
    path: Option<String>,
    staged: Option<bool>,
}
#[derive(Deserialize)]
struct LogArgs {
    count: Option<usize>,
}
#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}
#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}
#[derive(Deserialize)]
struct CommandArgs {
    command: String,
    timeout_secs: Option<u64>,
}
#[derive(Deserialize)]
struct TestArgs {
    filter: Option<String>,
}
#[derive(Deserialize)]
struct NoArgs {}
#[derive(Deserialize)]
struct FetchArgs {
    url: String,
}
#[derive(Deserialize)]
struct SkillArgs {
    name: String,
}

/// Validate a tool call and work out exactly what it would do.
pub fn plan(name: &str, raw_args: &str, ctx: &ToolCtx) -> Result<Plan, String> {
    let ws = &ctx.workspace;
    let kind = kind_of(name).ok_or_else(|| format!("unknown tool '{name}'"))?;
    let mk = |title: String, preview: Option<String>, danger: Option<String>, grant_key: String, action: Action| Plan {
        tool: name.to_string(),
        kind,
        title,
        preview,
        danger,
        grant_key,
        action,
    };
    match name {
        "read_file" => {
            let a: ReadArgs = args(name, raw_args)?;
            let (path, rel) = ws.resolve_existing(&a.path)?;
            if is_sensitive(&path) {
                return Err(format!("{rel} looks like a secret or credential file; reading it is not allowed"));
            }
            let offset = a.offset.unwrap_or(1).max(1);
            let limit = a.limit.unwrap_or(400).clamp(1, 2000);
            Ok(mk(format!("read {rel}"), None, None, "read".into(), Action::Read { path, rel, offset, limit }))
        }
        "list_dir" => {
            let a: ListArgs = args(name, raw_args)?;
            let (path, rel) = ws.resolve_existing(a.path.as_deref().unwrap_or("."))?;
            let depth = a.depth.unwrap_or(2).clamp(1, 4);
            Ok(mk(format!("list {rel}"), None, None, "read".into(), Action::List { path, rel, depth }))
        }
        "find_files" => {
            let a: FindArgs = args(name, raw_args)?;
            if a.pattern.trim().is_empty() {
                return Err("pattern must not be empty".into());
            }
            Ok(mk(format!("find {}", a.pattern), None, None, "read".into(), Action::Find { pattern: a.pattern }))
        }
        "grep" => {
            let a: GrepArgs = args(name, raw_args)?;
            regex::RegexBuilder::new(&a.pattern).build().map_err(|e| format!("invalid regex: {e}"))?;
            let (path, rel) = ws.resolve_existing(a.path.as_deref().unwrap_or("."))?;
            let title = format!("grep /{}/ in {rel}", a.pattern);
            Ok(mk(
                title,
                None,
                None,
                "read".into(),
                Action::Grep { pattern: a.pattern, path, glob: a.glob, case_insensitive: a.case_insensitive.unwrap_or(false) },
            ))
        }
        "project_info" => {
            let _: NoArgs = args(name, raw_args)?;
            Ok(mk("inspect project".into(), None, None, "read".into(), Action::Project))
        }
        "git_status" => {
            let _: NoArgs = args(name, raw_args)?;
            let a = ["status", "--short", "--branch"].map(String::from).to_vec();
            Ok(mk("git status".into(), None, None, "read".into(), Action::Git { args: a }))
        }
        "git_diff" => {
            let a: DiffArgs = args(name, raw_args)?;
            let mut g = vec!["diff".to_string(), "--no-color".into(), "--no-ext-diff".into()];
            if a.staged.unwrap_or(false) {
                g.push("--staged".into());
            }
            let mut title = "git diff".to_string();
            if let Some(p) = a.path {
                let (_, rel) = ws.resolve_lexical(&p)?;
                title.push_str(&format!(" {rel}"));
                g.push("--".into());
                g.push(rel);
            }
            Ok(mk(title, None, None, "read".into(), Action::Git { args: g }))
        }
        "git_log" => {
            let a: LogArgs = args(name, raw_args)?;
            let n = a.count.unwrap_or(10).clamp(1, 50);
            let g = vec!["log".into(), format!("-{n}"), "--no-color".into(), "--format=%h %ad %an%n    %s".into(), "--date=short".into()];
            Ok(mk(format!("git log -{n}"), None, None, "read".into(), Action::Git { args: g }))
        }
        "write_file" => {
            let a: WriteArgs = args(name, raw_args)?;
            let (path, rel) = ws.resolve_for_write(&a.path)?;
            let original = match std::fs::read(&path) {
                Ok(b) => Some(String::from_utf8(b).map_err(|_| format!("{rel} is not a UTF-8 text file"))?),
                Err(_) => None,
            };
            let preview = fsops::unified_diff(&rel, original.as_deref().unwrap_or(""), &a.content);
            let verb = if original.is_some() { "overwrite" } else { "create" };
            Ok(mk(format!("{verb} {rel}"), Some(preview), None, "write".into(), Action::Write { path, rel, content: a.content, original }))
        }
        "edit_file" => {
            let a: EditArgs = args(name, raw_args)?;
            let (path, rel) = ws.resolve_existing(&a.path)?;
            if !ws.writable(&path) {
                return Err(format!("{rel} is protected and cannot be edited"));
            }
            let original = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {rel}: {e}"))?;
            let updated = fsops::apply_edit(&original, &a.old_string, &a.new_string, a.replace_all.unwrap_or(false))?;
            let preview = fsops::unified_diff(&rel, &original, &updated);
            Ok(mk(format!("edit {rel}"), Some(preview), None, "write".into(), Action::Edit { path, rel, original, updated }))
        }
        "run_command" => {
            let a: CommandArgs = args(name, raw_args)?;
            let command = a.command.trim().to_string();
            if command.is_empty() {
                return Err("command must not be empty".into());
            }
            let timeout = Duration::from_secs(a.timeout_secs.unwrap_or(ctx.command_timeout.as_secs()).clamp(1, 600));
            let danger = danger(&command);
            Ok(mk(
                format!("run `{}`", one_line(&command)),
                Some(format!("$ {command}")),
                danger,
                format!("cmd:{command}"),
                Action::Command { command, timeout },
            ))
        }
        "run_tests" => {
            let a: TestArgs = args(name, raw_args)?;
            let base = project::test_command(ws.root()).ok_or("could not detect a test command for this project; use run_command")?;
            let command = match a.filter.filter(|f| !f.trim().is_empty()) {
                Some(f) => {
                    if f.chars().any(|c| !(c.is_alphanumeric() || "_-:./".contains(c))) {
                        return Err("filter may only contain letters, digits and _-:./".into());
                    }
                    format!("{base} {f}")
                }
                None => base,
            };
            Ok(mk(
                format!("run tests: `{command}`"),
                Some(format!("$ {command}")),
                None,
                format!("tests:{command}"),
                Action::Command { command, timeout: Duration::from_secs(600) },
            ))
        }
        "web_fetch" => {
            let a: FetchArgs = args(name, raw_args)?;
            let url = web::validate_url(&a.url)?;
            let host = url.host_str().unwrap_or("").to_string();
            Ok(mk(
                format!("fetch {}", one_line(url.as_str())),
                Some(url.to_string()),
                None,
                format!("web:{host}"),
                Action::Fetch { url: url.to_string() },
            ))
        }
        "load_skill" => {
            let a: SkillArgs = args(name, raw_args)?;
            let skill = ctx.skills.iter().find(|s| s.name.eq_ignore_ascii_case(a.name.trim())).cloned().ok_or_else(|| {
                format!(
                    "no skill named '{}'. Installed: {}",
                    a.name,
                    ctx.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
                )
            })?;
            Ok(mk(format!("load skill {}", skill.name), None, None, "skill".into(), Action::LoadSkill { skill }))
        }
        _ => Err(format!("unknown tool '{name}'")),
    }
}

fn one_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("");
    if l.chars().count() > 60 || s.contains('\n') { format!("{}…", l.chars().take(60).collect::<String>()) } else { l.to_string() }
}

/// Run an approved plan.
pub async fn execute(plan: &Plan, ctx: &ToolCtx) -> ToolOutput {
    let root = ctx.workspace.root().to_path_buf();
    let out = match &plan.action {
        Action::Read { path, rel, offset, limit } => fsops::read(path, rel, *offset, *limit),
        Action::List { path, rel, depth } => fsops::list(&root, path, rel, *depth),
        Action::Find { pattern } => fsops::find(&root, pattern),
        Action::Grep { pattern, path, glob, case_insensitive } => {
            let (root, pattern, path, glob, ci) = (root.clone(), pattern.clone(), path.clone(), glob.clone(), *case_insensitive);
            tokio::task::spawn_blocking(move || fsops::grep(&root, &pattern, &path, glob.as_deref(), ci))
                .await
                .unwrap_or_else(|e| ToolOutput::error(e.to_string()))
        }
        Action::Project => {
            let r = root.clone();
            tokio::task::spawn_blocking(move || project::info(&r)).await.unwrap_or_else(|e| ToolOutput::error(e.to_string()))
        }
        Action::Git { args } => exec::git(&root, args, &ctx.cancel).await,
        Action::Write { path, rel, content, original } => fsops::write(path, rel, content, original.as_deref()),
        Action::Edit { path, rel, original, updated } => fsops::edit(path, rel, original, updated),
        Action::Command { command, timeout } => exec::run_command(&root, command, *timeout, &ctx.scrub_env, &ctx.cancel).await,
        Action::Fetch { url } => match &ctx.http {
            None => ToolOutput::error("web access is disabled"),
            Some(c) => tokio::select! {
                r = web::fetch(c, url) => match r {
                    Ok(page) => {
                        let n = page.text.chars().count();
                        ToolOutput::ok(page.render(), format!("{} · {} chars", page.title.chars().take(50).collect::<String>(), n))
                    }
                    Err(e) => ToolOutput::error(e),
                },
                _ = ctx.cancel.cancelled() => ToolOutput::error("cancelled"),
            },
        },
        Action::LoadSkill { skill } => load_skill(skill),
    };
    ToolOutput { content: truncate_middle(&out.content, MAX_RESULT_CHARS), ..out }
}

fn load_skill(skill: &SkillRef) -> ToolOutput {
    let md = skill.dir.join("SKILL.md");
    let text = match std::fs::read_to_string(&md) {
        Ok(t) => t,
        Err(e) => return ToolOutput::error(format!("cannot read {}: {e}", md.display())),
    };
    // List bundled resources so the model knows what else it can read or run.
    let mut files = Vec::new();
    for e in ignore::WalkBuilder::new(&skill.dir).max_depth(Some(3)).build().flatten().take(200) {
        if e.file_type().is_some_and(|t| t.is_file()) && e.path() != md {
            files.push(e.path().display().to_string());
        }
    }
    let mut out = format!("Skill '{}' (directory: {})\n\n{text}", skill.name, skill.dir.display());
    if !files.is_empty() {
        out.push_str(&format!("\n\nBundled files (read them with run_command, e.g. `cat <path>`, only if needed):\n{}", files.join("\n")));
    }
    ToolOutput::ok(out, format!("{} lines", text.lines().count()))
}

/// Keep the beginning and end of long output; the middle is usually least useful.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max * 2 / 3).collect();
    let tail_n = max / 3;
    let total = s.chars().count();
    let tail: String = s.chars().skip(total - tail_n).collect();
    format!("{head}\n… [{} characters omitted] …\n{tail}", total - head.chars().count() - tail_n)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn ctx(root: &std::path::Path) -> ToolCtx {
        ToolCtx {
            workspace: Workspace::new(root).unwrap(),
            cancel: CancellationToken::new(),
            command_timeout: Duration::from_secs(30),
            scrub_env: vec!["TWO_ONE_TWO_TEST_SECRET".into()],
            skills: vec![],
            http: None,
        }
    }

    #[test]
    fn definitions_filter_by_kind() {
        let read = definitions(&[ToolKind::Read]);
        assert!(read.iter().all(|d| kind_of(d["function"]["name"].as_str().unwrap()) == Some(ToolKind::Read)));
        let all = [ToolKind::Read, ToolKind::Write, ToolKind::Execute, ToolKind::Network, ToolKind::Skill];
        assert_eq!(definitions(&all).len(), SPECS.len());
        for d in definitions(&[ToolKind::Read, ToolKind::Write, ToolKind::Execute]) {
            assert_eq!(d["function"]["parameters"]["type"], "object");
        }
    }

    #[test]
    fn rejects_bad_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        assert!(plan("read_file", "{}", &c).unwrap_err().contains("missing field `path`"));
        assert!(plan("read_file", "not json", &c).unwrap_err().contains("invalid arguments"));
        assert!(plan("read_file", r#"{"path": 5}"#, &c).is_err());
        assert!(plan("nuke", "{}", &c).unwrap_err().contains("unknown tool"));
        assert!(plan("grep", r#"{"pattern":"("}"#, &c).unwrap_err().contains("invalid regex"));
        assert!(plan("run_command", r#"{"command":"  "}"#, &c).is_err());
        // Empty argument strings are accepted for argument-less tools.
        assert!(plan("git_status", "", &c).is_ok());
    }

    #[test]
    fn truncation_keeps_head_and_tail() {
        let s: String = (0..1000).map(|i| format!("{i}\n")).collect();
        let t = truncate_middle(&s, 300);
        assert!(t.starts_with("0\n1\n"));
        assert!(t.ends_with("999\n"));
        assert!(t.contains("omitted"));
        assert!(t.chars().count() < 400);
    }
}
