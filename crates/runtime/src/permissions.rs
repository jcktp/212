//! Tool permission policy and the audit log.
//!
//! | mode  | read            | write                 | execute                                   |
//! |-------|-----------------|-----------------------|-------------------------------------------|
//! | chat  | not offered     | not offered           | not offered                               |
//! | plan  | allowed         | denied                | denied                                    |
//! | build | allowed         | ask (or session grant)| ask (or session grant); risky: always ask |
//!
//! Reads are confined to the workspace by the tools themselves. "Allow for
//! session" is never offered for commands flagged as risky.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use common::proto::{AgentMode, Event, NoticeLevel};
use tools::{Plan, ToolKind};

use crate::{PendingAction, Runtime};

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Deny(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Policy {
    Allow,
    Ask { session_option: bool },
    Deny(String),
}

/// Tool kinds offered to the model in each mode.
pub fn offered(mode: AgentMode) -> &'static [ToolKind] {
    match mode {
        AgentMode::Chat => &[ToolKind::Network, ToolKind::Skill],
        AgentMode::Plan => &[ToolKind::Read, ToolKind::Network, ToolKind::Skill],
        AgentMode::Build => &[ToolKind::Read, ToolKind::Write, ToolKind::Execute, ToolKind::Network, ToolKind::Skill],
    }
}

/// Kinds actually offered given configuration (web access, installed skills).
pub fn offered_with(mode: AgentMode, web_access: &str, have_skills: bool) -> Vec<ToolKind> {
    offered(mode)
        .iter()
        .copied()
        .filter(|k| !(*k == ToolKind::Network && web_access == "off") && !(*k == ToolKind::Skill && !have_skills))
        .collect()
}

pub fn decide(mode: AgentMode, plan: &Plan, grants: &HashSet<String>, require_command_approval: bool) -> Policy {
    decide_with(mode, plan, grants, require_command_approval, "ask")
}

pub fn decide_with(mode: AgentMode, plan: &Plan, grants: &HashSet<String>, require_command_approval: bool, web_access: &str) -> Policy {
    if !offered(mode).contains(&plan.kind) {
        return Policy::Deny(format!("{} is not allowed in {} mode", plan.tool, mode.as_str()));
    }
    match plan.kind {
        ToolKind::Read | ToolKind::Skill => Policy::Allow,
        ToolKind::Network => match web_access {
            "off" => Policy::Deny("web access is turned off (agent.web_access)".into()),
            "allow" => Policy::Allow,
            _ if grants.contains(&plan.grant_key) => Policy::Allow,
            _ => Policy::Ask { session_option: true },
        },
        ToolKind::Write => {
            if grants.contains(&plan.grant_key) {
                Policy::Allow
            } else {
                Policy::Ask { session_option: true }
            }
        }
        ToolKind::Execute => {
            if plan.danger.is_some() {
                Policy::Ask { session_option: false }
            } else if !require_command_approval || grants.contains(&plan.grant_key) {
                Policy::Allow
            } else {
                Policy::Ask { session_option: true }
            }
        }
    }
}

/// Append one JSON line to the audit log. Failures are logged, not fatal.
pub fn audit(path: &Path, tool: &str, title: &str, decision: &str, ok: Option<bool>, summary: &str) {
    let ts = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
    let line = serde_json::json!({"ts": ts, "tool": tool, "action": title, "decision": decision, "ok": ok, "result": summary});
    let r = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = r {
        tracing::warn!("audit log: {e}");
    }
}

impl Runtime {
    pub(crate) async fn on_permission(&mut self, plan: Box<Plan>, reply: tokio::sync::oneshot::Sender<Decision>) {
        match decide_with(self.mode, &plan, &self.grants, self.config.agent.require_command_approval, &self.config.agent.web_access) {
            Policy::Allow => {
                let _ = reply.send(Decision::Allow);
            }
            Policy::Deny(reason) => {
                let _ = reply.send(Decision::Deny(reason));
            }
            Policy::Ask { session_option } => {
                let mut body = Vec::new();
                if let Some(d) = &plan.danger {
                    body.push(format!("⚠ Risky: {d}. Review carefully."));
                }
                body.push(match plan.kind {
                    ToolKind::Write => "The model wants to change this file:".to_string(),
                    ToolKind::Network => "The model wants to read this web page (its content will be sent to the model):".to_string(),
                    _ => format!("The model wants to run this in {}:", self.cwd.display()),
                });
                let mut options = vec!["Allow once".to_string()];
                if session_option {
                    options.push(match plan.kind {
                        ToolKind::Write => "Allow all edits this session".into(),
                        ToolKind::Network => format!("Allow {} this session", plan.grant_key.trim_start_matches("web:")),
                        _ => "Always allow this command this session".into(),
                    });
                }
                options.push("Deny".into());
                let title = format!("Allow: {}?", plan.title);
                let preview = plan.preview.clone();
                let id = self.ask(PendingAction::Permission { plan, reply, session_option });
                // The deny option is always last.
                self.emit(Event::Choice { id, title, body, options, preview }).await;
            }
        }
    }

    /// Deny and close every open permission prompt (on cancel/new conversation).
    pub(crate) async fn dismiss_permissions(&mut self) {
        let ids: Vec<u64> = self.pending.iter().filter(|(_, a)| matches!(a, PendingAction::Permission { .. })).map(|(id, _)| *id).collect();
        for id in ids {
            if let Some(PendingAction::Permission { reply, .. }) = self.pending.remove(&id) {
                let _ = reply.send(Decision::Deny("cancelled".into()));
            }
            self.emit(Event::Dismiss { id }).await;
        }
    }

    pub(crate) async fn warn_no_tools(&mut self, model: &str) {
        if self.warned_no_tools.insert(model.to_string()) {
            self.notice(
                NoticeLevel::Warn,
                format!(
                    "{model}'s chat template does not declare tool support, so it runs without tools in {} mode. \
                     For agent work try a tool-capable model (e.g. Qwen3 4B/8B — see /recommend).",
                    self.mode.as_str()
                ),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn plan(tmp: &Path, tool: &str, args: &str) -> Plan {
        let ctx = tools::ToolCtx {
            workspace: tools::Workspace::new(tmp).unwrap(),
            cancel: CancellationToken::new(),
            command_timeout: Duration::from_secs(5),
            scrub_env: vec![],
            skills: vec![],
            http: None,
        };
        tools::plan(tool, args, &ctx).unwrap()
    }

    #[test]
    fn policy_matrix() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
        let read = plan(tmp.path(), "read_file", r#"{"path":"a.txt"}"#);
        let edit = plan(tmp.path(), "edit_file", r#"{"path":"a.txt","old_string":"x","new_string":"y"}"#);
        let cmd = plan(tmp.path(), "run_command", r#"{"command":"ls"}"#);
        let risky = plan(tmp.path(), "run_command", r#"{"command":"rm -rf build"}"#);
        let none = HashSet::new();
        use AgentMode::*;
        assert!(matches!(decide(Chat, &read, &none, true), Policy::Deny(_)));
        assert_eq!(decide(Plan, &read, &none, true), Policy::Allow);
        assert!(matches!(decide(Plan, &edit, &none, true), Policy::Deny(r) if r.contains("plan mode")));
        assert!(matches!(decide(Plan, &cmd, &none, true), Policy::Deny(_)));
        assert_eq!(decide(Build, &read, &none, true), Policy::Allow);
        assert_eq!(decide(Build, &edit, &none, true), Policy::Ask { session_option: true });
        assert_eq!(decide(Build, &cmd, &none, true), Policy::Ask { session_option: true });
        assert_eq!(decide(Build, &cmd, &none, false), Policy::Allow);
        // Risky commands always ask and can't be granted for the session.
        assert_eq!(decide(Build, &risky, &none, false), Policy::Ask { session_option: false });
        let grants: HashSet<String> = [edit.grant_key.clone(), cmd.grant_key.clone(), risky.grant_key.clone()].into();
        assert_eq!(decide(Build, &edit, &grants, true), Policy::Allow);
        assert_eq!(decide(Build, &cmd, &grants, true), Policy::Allow);
        assert_eq!(decide(Build, &risky, &grants, true), Policy::Ask { session_option: false });
        // Web: ask per site, allow/off by config, session grants per host.
        let web = plan(tmp.path(), "web_fetch", r#"{"url":"https://docs.rs/x"}"#);
        assert_eq!(decide_with(Chat, &web, &none, true, "ask"), Policy::Ask { session_option: true });
        assert_eq!(decide_with(Chat, &web, &none, true, "allow"), Policy::Allow);
        assert!(matches!(decide_with(Build, &web, &none, true, "off"), Policy::Deny(_)));
        let host: HashSet<String> = ["web:docs.rs".to_string()].into();
        assert_eq!(decide_with(Plan, &web, &host, true, "ask"), Policy::Allow);
        assert_eq!(offered_with(Chat, "off", false), Vec::<ToolKind>::new());
        assert_eq!(offered_with(Chat, "ask", true), vec![ToolKind::Network, ToolKind::Skill]);
        // A grant for one command doesn't cover another.
        let other = plan(tmp.path(), "run_command", r#"{"command":"ls -la"}"#);
        assert_eq!(decide(Build, &other, &grants, true), Policy::Ask { session_option: true });
    }

    #[test]
    fn audit_appends_json_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("audit.jsonl");
        audit(&p, "edit_file", "edit a.txt", "allowed", Some(true), "+1 -1");
        audit(&p, "run_command", "run `ls`", "denied", None, "");
        let lines: Vec<serde_json::Value> =
            std::fs::read_to_string(&p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["tool"], "edit_file");
        assert_eq!(lines[1]["decision"], "denied");
    }
}
