//! Sending messages and streaming replies.

use std::time::Instant;

use common::proto::{Event, InferenceState, NoticeLevel};
use common::{Message, Role, StreamEvent};
use futures::StreamExt;
use providers::{ChatRequest, Sampling};
use tokio_util::sync::CancellationToken;

use crate::{Internal, Runtime, context};

impl Runtime {
    fn system_prompt(&self) -> Message {
        let custom = self.config.inference.system_prompt.clone();
        Message::system(custom.unwrap_or_else(|| {
            format!(
                "You are 212, a local-first AI assistant running in the user's terminal. \
                 Answer clearly and concisely. Use Markdown, and fenced code blocks with a language tag for code. \
                 The user's working directory is {}.",
                self.cwd.display()
            )
        }))
    }

    pub(crate) async fn send(&mut self, text: String) {
        if self.generation.is_some() {
            return self.notice(NoticeLevel::Warn, "Still generating — press Esc to stop it first.").await;
        }
        let Some(provider) = self.provider.clone() else {
            if self.loading.is_some() {
                self.queued_send = Some(text);
                return self.notice(NoticeLevel::Info, "The model is still loading; your message will be sent when it's ready.").await;
            }
            let msg = if self.provider_name != "local" {
                format!("The {} provider is not ready. Check /provider and /doctor.", self.provider_name)
            } else if self.store.list().is_empty() {
                "No local model installed yet. Try /recommend, /models search <name>, or /models import <file.gguf>.".to_string()
            } else {
                "No model loaded. Pick one with /models.".to_string()
            };
            return self.notice(NoticeLevel::Warn, msg).await;
        };

        let (text, outcomes) = crate::attach::expand(&text, &self.cwd);
        for o in outcomes {
            match o {
                crate::attach::Outcome::Attached { path, bytes, truncated } => {
                    let t = if truncated {
                        format!(" (first {} attached)", common::format::bytes(crate::attach::MAX_FILE_BYTES))
                    } else {
                        String::new()
                    };
                    self.notice(NoticeLevel::Info, format!("Attached {path} · {}{t}", common::format::bytes(bytes as u64))).await;
                }
                crate::attach::Outcome::Refused { path, reason } => {
                    self.notice(NoticeLevel::Warn, format!("Not attaching {path}: {reason}")).await
                }
            }
        }
        self.messages.push(Message::user(text));
        let ctx = self.context_size();
        let reserve = self.config.inference.max_tokens.unwrap_or(ctx / 4).min(ctx / 2);
        let (messages, dropped) = context::select(Some(&self.system_prompt()), &self.messages, ctx.saturating_sub(reserve));
        if dropped > 0 {
            self.notice(
                NoticeLevel::Info,
                format!("Context: {dropped} older message(s) were left out to fit the {ctx}-token window. /new starts fresh."),
            )
            .await;
        }
        let req = ChatRequest {
            model: self.model_name.clone().unwrap_or_default(),
            messages,
            sampling: Sampling {
                temperature: Some(self.config.inference.temperature),
                top_p: Some(self.config.inference.top_p),
                max_tokens: self.config.inference.max_tokens,
            },
            tools: Vec::new(),
        };

        let cancel = CancellationToken::new();
        self.generation = Some(cancel.clone());
        self.emit(Event::Inference(InferenceState::Generating)).await;
        self.emit(Event::TurnStart).await;
        let events = self.events.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let start = Instant::now();
            let mut first_token: Option<Instant> = None;
            let mut stream = provider.chat(req);
            let (mut text, mut reasoning) = (String::new(), String::new());
            let (mut chunks, mut prompt_tokens, mut completion_tokens, mut speed) = (0u64, None, None, None);
            let mut cancelled = false;
            let mut failed = false;
            loop {
                let next = tokio::select! {
                    n = stream.next() => n,
                    _ = cancel.cancelled() => { cancelled = true; break; }
                };
                match next {
                    Some(Ok(ev)) => {
                        match &ev {
                            StreamEvent::Text(t) => {
                                first_token.get_or_insert_with(Instant::now);
                                chunks += 1;
                                text.push_str(t);
                            }
                            StreamEvent::Reasoning(r) => {
                                first_token.get_or_insert_with(Instant::now);
                                chunks += 1;
                                reasoning.push_str(r);
                            }
                            StreamEvent::Usage(u) => {
                                if u.prompt_tokens > 0 {
                                    prompt_tokens = Some(u.prompt_tokens);
                                }
                                if u.completion_tokens > 0 {
                                    completion_tokens = Some(u.completion_tokens);
                                }
                            }
                            StreamEvent::Speed { tokens_per_second } => speed = Some(*tokens_per_second),
                            _ => {}
                        }
                        let _ = events.send(Event::Delta(ev)).await;
                    }
                    Some(Err(e)) => {
                        failed = true;
                        let _ = events.send(Event::Notice { level: NoticeLevel::Error, text: e.to_string() }).await;
                        break;
                    }
                    None => break,
                }
            }
            drop(stream); // closes the connection; the server stops generating
            let completion = completion_tokens.unwrap_or(chunks);
            let tps = speed.or_else(|| {
                let secs = first_token?.elapsed().as_secs_f64();
                (secs > 0.2 && chunks > 1).then(|| chunks as f64 / secs)
            });
            let _ = events
                .send(Event::TurnEnd {
                    completion_tokens: completion,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    tokens_per_second: tps,
                    cancelled,
                })
                .await;
            let empty = text.is_empty() && reasoning.is_empty();
            let message = (!empty).then(|| {
                let mut m = Message::new(Role::Assistant, text);
                m.reasoning = (!reasoning.is_empty()).then_some(reasoning);
                m
            });
            let _ = tx
                .send(Internal::Generated {
                    message,
                    prompt_tokens,
                    completion_tokens: completion,
                    failed_empty: empty && (failed || cancelled),
                })
                .await;
        });
    }
}
