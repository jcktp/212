//! The agent loop.
//!
//! One user turn = up to `max_steps` model calls. After each call, any tool
//! calls are validated, passed through the permission policy (which may ask
//! the user), executed and fed back as tool results. The loop ends when the
//! model answers without calling tools, on cancellation, on an unrecoverable
//! error, or at the step limit.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::proto::{Event, NoticeLevel};
use common::{Message, Role, StreamEvent, ToolCall};
use futures::StreamExt;
use providers::{ChatRequest, Provider, ProviderError, Sampling};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tools::{Plan, ToolCtx, ToolKind};

use crate::Internal;
use crate::context::message_tokens;
use crate::permissions::{Decision, audit};

const MAX_RETRIES: usize = 2;
/// Older tool results are shortened to this when the context fills up.
const SHRUNK_RESULT_CHARS: usize = 1200;

pub(crate) struct AgentRun {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub sampling: Sampling,
    /// System prompt + selected history, ending with the new user message.
    pub messages: Vec<Message>,
    pub tool_defs: Vec<serde_json::Value>,
    pub ctx: ToolCtx,
    pub max_steps: u32,
    pub budget: u32,
    pub events: mpsc::Sender<Event>,
    pub internal: mpsc::Sender<Internal>,
    pub cancel: CancellationToken,
    pub audit_path: PathBuf,
}

#[derive(Default)]
struct Step {
    text: String,
    reasoning: String,
    calls: BTreeMap<usize, (Option<String>, String, String)>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    chunks: u64,
    speed: Option<f64>,
    error: Option<ProviderError>,
    cancelled: bool,
}

impl AgentRun {
    async fn emit(&self, e: Event) {
        let _ = self.events.send(e).await;
    }

    async fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.emit(Event::Notice { level, text: text.into() }).await;
    }

    async fn stream_step(&self, messages: &[Message], first_token: &mut Option<Instant>) -> Step {
        let req = ChatRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            sampling: self.sampling.clone(),
            tools: self.tool_defs.clone(),
        };
        let mut stream = self.provider.chat(req);
        let mut s = Step::default();
        loop {
            let next = tokio::select! {
                n = stream.next() => n,
                _ = self.cancel.cancelled() => { s.cancelled = true; break; }
            };
            match next {
                Some(Ok(ev)) => {
                    match &ev {
                        StreamEvent::Text(t) => {
                            first_token.get_or_insert_with(Instant::now);
                            s.chunks += 1;
                            s.text.push_str(t);
                        }
                        StreamEvent::Reasoning(r) => {
                            first_token.get_or_insert_with(Instant::now);
                            s.chunks += 1;
                            s.reasoning.push_str(r);
                        }
                        StreamEvent::ToolCallDelta { index, id, name, arguments } => {
                            s.chunks += 1;
                            let e = s.calls.entry(*index).or_insert((None, String::new(), String::new()));
                            if id.is_some() {
                                e.0 = id.clone();
                            }
                            if let Some(n) = name {
                                e.1.push_str(n);
                            }
                            e.2.push_str(arguments);
                            continue; // the UI shows tool calls via ToolStart
                        }
                        StreamEvent::Usage(u) => {
                            if u.prompt_tokens > 0 {
                                s.prompt_tokens = Some(u.prompt_tokens);
                            }
                            if u.completion_tokens > 0 {
                                s.completion_tokens = Some(u.completion_tokens);
                            }
                        }
                        StreamEvent::Speed { tokens_per_second } => s.speed = Some(*tokens_per_second),
                        StreamEvent::Done { .. } => {}
                    }
                    self.emit(Event::Delta(ev)).await;
                }
                Some(Err(e)) => {
                    s.error = Some(e);
                    break;
                }
                None => break,
            }
        }
        s
    }

    pub async fn run(mut self, mut prompt_tokens: Option<u64>) {
        let start = Instant::now();
        let mut first_token = None;
        let mut new_msgs: Vec<Message> = Vec::new();
        let (mut completion_total, mut chunks_total, mut speed) = (0u64, 0u64, None);
        let mut cancelled = false;
        let mut failed = false;
        let mut shrunk_notice = false;
        let tools_enabled = !self.tool_defs.is_empty();
        let mut hit_limit = true;

        'steps: for step in 0..self.max_steps {
            if self.cancel.is_cancelled() {
                cancelled = true;
                hit_limit = false;
                break;
            }
            // Keep the turn inside the context window by shortening old tool output.
            if tools_enabled && self.messages.iter().map(message_tokens).sum::<u32>() > self.budget {
                let last_assistant = self.messages.iter().rposition(|m| m.role == Role::Assistant).unwrap_or(0);
                for m in self.messages[..last_assistant].iter_mut().filter(|m| m.role == Role::Tool) {
                    if m.content.chars().count() > SHRUNK_RESULT_CHARS {
                        m.content =
                            format!("{}\n… [shortened to save context]", m.content.chars().take(SHRUNK_RESULT_CHARS).collect::<String>());
                    }
                }
                if !shrunk_notice {
                    shrunk_notice = true;
                    self.notice(NoticeLevel::Info, "Context is filling up: older tool results in this turn were shortened.").await;
                }
            }

            let mut attempt = 0;
            let s = loop {
                let s = self.stream_step(&self.messages, &mut first_token).await;
                let retryable = matches!(s.error, Some(ProviderError::Connect { .. })) && s.chunks == 0 && !s.cancelled;
                if retryable && attempt < MAX_RETRIES {
                    attempt += 1;
                    tracing::warn!(attempt, "provider connection failed; retrying");
                    tokio::time::sleep(Duration::from_millis(800 * attempt as u64)).await;
                    continue;
                }
                break s;
            };
            if s.prompt_tokens.is_some() {
                prompt_tokens = s.prompt_tokens;
            }
            completion_total += s.completion_tokens.unwrap_or(s.chunks);
            chunks_total += s.chunks;
            speed = s.speed.or(speed);
            if let Some(e) = &s.error {
                self.notice(NoticeLevel::Error, e.to_string()).await;
                failed = true;
            }
            cancelled = s.cancelled;

            // Assemble tool calls: native first, then the labelled fallback.
            let mut text = s.text;
            let mut calls: Vec<ToolCall> = s
                .calls
                .into_iter()
                .filter(|(_, (_, name, _))| !name.is_empty())
                .map(|(i, (id, name, arguments))| ToolCall { id: id.unwrap_or_else(|| format!("call_{step}_{i}")), name, arguments })
                .collect();
            if tools_enabled && calls.is_empty() && !cancelled {
                let (rest, parsed) = fallback_tool_calls(&text, step);
                if !parsed.is_empty() {
                    tracing::info!(n = parsed.len(), "parsed tool calls from text (compatibility fallback)");
                    text = rest;
                    calls = parsed;
                }
            }
            if text.is_empty() && s.reasoning.is_empty() && calls.is_empty() {
                hit_limit = false;
                break;
            }
            let mut assistant = Message::new(Role::Assistant, text);
            assistant.reasoning = (!s.reasoning.is_empty()).then_some(s.reasoning);
            assistant.tool_calls = if cancelled || failed { Vec::new() } else { calls.clone() };
            self.messages.push(assistant.clone());
            new_msgs.push(assistant);
            if calls.is_empty() || cancelled || failed {
                hit_limit = false;
                break;
            }

            // Plan every call, ask for permission, then execute. Reads that
            // were approved run concurrently; everything else runs in order.
            let mut results: Vec<Option<(String, tools::ToolOutput)>> = vec![None; calls.len()];
            let mut approved: Vec<(usize, Plan)> = Vec::new();
            for (i, call) in calls.iter().enumerate() {
                let plan = match tools::plan(&call.name, &call.arguments, &self.ctx) {
                    Ok(p) => p,
                    Err(e) => {
                        self.emit(Event::ToolStart { id: call.id.clone(), tool: call.name.clone(), title: call.name.clone() }).await;
                        let out = tools::ToolOutput::error(e);
                        self.emit(Event::ToolEnd { id: call.id.clone(), ok: false, summary: out.summary.clone(), detail: None }).await;
                        audit(&self.audit_path, &call.name, &call.arguments, "invalid", Some(false), &out.summary);
                        results[i] = Some((call.id.clone(), out));
                        continue;
                    }
                };
                self.emit(Event::ToolStart { id: call.id.clone(), tool: plan.tool.clone(), title: plan.title.clone() }).await;
                let (reply_tx, reply_rx) = oneshot::channel();
                if self.internal.send(Internal::Permission { plan: Box::new(plan.clone()), reply: reply_tx }).await.is_err() {
                    break 'steps;
                }
                let decision = tokio::select! {
                    d = reply_rx => d.unwrap_or(Decision::Deny("no answer".into())),
                    _ = self.cancel.cancelled() => Decision::Deny("cancelled".into()),
                };
                match decision {
                    Decision::Allow => approved.push((i, plan)),
                    Decision::Deny(reason) => {
                        audit(&self.audit_path, &plan.tool, &plan.title, &format!("denied: {reason}"), None, "");
                        let out = tools::ToolOutput::error(format!(
                            "not run: {reason}. Do not retry the same action; adjust your approach or ask the user."
                        ));
                        self.emit(Event::ToolEnd { id: call.id.clone(), ok: false, summary: format!("denied — {reason}"), detail: None })
                            .await;
                        results[i] = Some((call.id.clone(), out));
                    }
                }
                if self.cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
            }

            if !cancelled {
                let (reads, others): (Vec<_>, Vec<_>) = approved.into_iter().partition(|(_, p)| p.kind == ToolKind::Read);
                let read_outs = futures::future::join_all(reads.iter().map(|(_, p)| tools::execute(p, &self.ctx))).await;
                for ((i, plan), out) in reads.iter().zip(read_outs) {
                    self.finish_tool(&calls[*i], plan, &out).await;
                    results[*i] = Some((calls[*i].id.clone(), out));
                }
                for (i, plan) in others {
                    if self.cancel.is_cancelled() {
                        cancelled = true;
                        break;
                    }
                    let out = tools::execute(&plan, &self.ctx).await;
                    self.finish_tool(&calls[i], &plan, &out).await;
                    results[i] = Some((calls[i].id.clone(), out));
                }
            }

            if self.cancel.is_cancelled() {
                cancelled = true;
            }
            // Every tool call gets exactly one result message.
            for (i, call) in calls.iter().enumerate() {
                let (id, out) = results[i].take().unwrap_or_else(|| (call.id.clone(), tools::ToolOutput::error("cancelled by the user")));
                let mut m = Message::new(Role::Tool, out.content);
                m.tool_call_id = Some(id);
                self.messages.push(m.clone());
                new_msgs.push(m);
            }
            if cancelled {
                hit_limit = false;
                break;
            }
        }
        if hit_limit && !cancelled && !failed {
            self.notice(
                NoticeLevel::Warn,
                format!("Stopped after {} steps (agent.max_steps). Say \"continue\" to let the agent keep going.", self.max_steps),
            )
            .await;
        }

        let tps = speed.or_else(|| {
            let secs = first_token?.elapsed().as_secs_f64();
            (secs > 0.2 && chunks_total > 1).then(|| chunks_total as f64 / secs)
        });
        self.emit(Event::TurnEnd {
            completion_tokens: completion_total,
            elapsed_ms: start.elapsed().as_millis() as u64,
            tokens_per_second: tps,
            cancelled,
        })
        .await;
        let empty = new_msgs.is_empty();
        let _ = self
            .internal
            .send(Internal::Generated {
                messages: new_msgs,
                prompt_tokens,
                completion_tokens: completion_total,
                failed_empty: empty && (failed || cancelled),
            })
            .await;
    }

    async fn finish_tool(&self, call: &ToolCall, plan: &Plan, out: &tools::ToolOutput) {
        let detail = match plan.kind {
            ToolKind::Write => plan.preview.clone(),
            ToolKind::Execute => {
                let lines: Vec<&str> = out.content.lines().collect();
                Some(lines[lines.len().saturating_sub(14)..].join("\n"))
            }
            ToolKind::Read => (!out.ok).then(|| out.content.clone()),
        };
        audit(&self.audit_path, &plan.tool, &plan.title, "allowed", Some(out.ok), &out.summary);
        self.emit(Event::ToolEnd { id: call.id.clone(), ok: out.ok, summary: out.summary.clone(), detail }).await;
    }
}

/// Compatibility fallback for models whose tool calls arrive as text
/// (`<tool_call>{"name": …, "arguments": {…}}</tool_call>`) because the server
/// didn't parse them. Results are validated like native calls.
pub fn fallback_tool_calls(text: &str, step: u32) -> (String, Vec<ToolCall>) {
    let mut rest = String::new();
    let mut calls = Vec::new();
    let mut s = text;
    while let Some(start) = s.find("<tool_call>") {
        let Some(end_rel) = s[start..].find("</tool_call>") else { break };
        let body = s[start + "<tool_call>".len()..start + end_rel].trim();
        rest.push_str(&s[..start]);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body)
            && let Some(name) = v.get("name").and_then(|n| n.as_str())
        {
            let args = match v.get("arguments").or_else(|| v.get("parameters")) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => "{}".into(),
            };
            calls.push(ToolCall { id: format!("call_{step}_t{}", calls.len()), name: name.to_string(), arguments: args });
        } else {
            rest.push_str(&s[start..start + end_rel + "</tool_call>".len()]);
        }
        s = &s[start + end_rel + "</tool_call>".len()..];
    }
    rest.push_str(s);
    (rest.trim().to_string(), calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_tool_calls() {
        let t = "Let me look.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}\n</tool_call>";
        let (rest, calls) = fallback_tool_calls(t, 0);
        assert_eq!(rest, "Let me look.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"a.rs"}"#);
        let (rest, calls) = fallback_tool_calls("no calls <tool_call>garbage</tool_call>", 0);
        assert!(calls.is_empty());
        assert!(rest.contains("garbage"));
    }
}
