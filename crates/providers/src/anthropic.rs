//! Anthropic Messages API streaming.

use std::collections::VecDeque;
use std::time::Duration;

use common::{Role, StreamEvent, Usage};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};

use crate::openai::root_cause;
use crate::sse::SseParser;
use crate::{ChatRequest, EventStream, Provider, ProviderError, RemoteModel, status_error};

pub const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8192;

#[derive(Clone)]
pub struct Anthropic {
    base_url: String,
    api_key: String,
    key_hint: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Anthropic").field("base_url", &self.base_url).finish()
    }
}

impl Anthropic {
    pub fn new(base_url: &str, api_key: String, key_hint: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            key_hint: key_hint.into(),
            client: {
                crate::init_tls();
                reqwest::Client::builder()
            }
            .user_agent(concat!("212/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("client"),
        }
    }

    pub fn body(req: &ChatRequest) -> Value {
        let system: Vec<&str> = req.messages.iter().filter(|m| m.role == Role::System).map(|m| m.content.as_str()).collect();
        let mut messages: Vec<Value> = Vec::new();
        for m in req.messages.iter().filter(|m| m.role != Role::System) {
            let (role, content) = match m.role {
                Role::User => ("user", json!([{"type": "text", "text": m.content}])),
                Role::Assistant => {
                    let mut blocks = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(json!({"type": "text", "text": m.content}));
                    }
                    for c in &m.tool_calls {
                        let input: Value = serde_json::from_str(&c.arguments).unwrap_or(json!({}));
                        blocks.push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": input}));
                    }
                    ("assistant", Value::Array(blocks))
                }
                Role::Tool => (
                    "user",
                    json!([{"type": "tool_result", "tool_use_id": m.tool_call_id.clone().unwrap_or_default(), "content": m.content}]),
                ),
                Role::System => unreachable!(),
            };
            // Merge consecutive same-role turns (e.g. several tool results).
            match messages.last_mut() {
                Some(last) if last["role"] == role => {
                    if let (Some(a), Some(b)) = (last["content"].as_array_mut(), content.as_array()) {
                        a.extend(b.iter().cloned());
                    }
                }
                _ => messages.push(json!({"role": role, "content": content})),
            }
        }
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.sampling.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "messages": messages,
            "stream": true,
        });
        if !system.is_empty() {
            body["system"] = json!(system.join("\n\n"));
        }
        if let Some(t) = req.sampling.temperature {
            body["temperature"] = json!(t.min(1.0));
        }
        if !req.tools.is_empty() {
            // Convert OpenAI function tools to Anthropic tool definitions.
            body["tools"] = req
                .tools
                .iter()
                .filter_map(|t| {
                    let f = t.get("function")?;
                    Some(json!({"name": f.get("name")?, "description": f.get("description").cloned().unwrap_or(json!("")), "input_schema": f.get("parameters").cloned().unwrap_or(json!({"type":"object"}))}))
                })
                .collect();
        }
        body
    }
}

pub fn parse_event(event: Option<&str>, data: &str, out: &mut VecDeque<StreamEvent>) -> Result<bool, String> {
    let v: Value = serde_json::from_str(data).map_err(|e| e.to_string())?;
    let kind = event.or_else(|| v.get("type").and_then(Value::as_str)).unwrap_or("");
    match kind {
        "message_start" => {
            if let Some(n) = v.pointer("/message/usage/input_tokens").and_then(Value::as_u64) {
                out.push_back(StreamEvent::Usage(Usage { prompt_tokens: n, completion_tokens: 0 }));
            }
        }
        "content_block_start" => {
            let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if v.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                out.push_back(StreamEvent::ToolCallDelta {
                    index: idx,
                    id: v.pointer("/content_block/id").and_then(Value::as_str).map(str::to_string),
                    name: v.pointer("/content_block/name").and_then(Value::as_str).map(str::to_string),
                    arguments: String::new(),
                });
            }
        }
        "content_block_delta" => {
            let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let d = v.get("delta").unwrap_or(&Value::Null);
            match d.get("type").and_then(Value::as_str) {
                Some("text_delta") => out.push_back(StreamEvent::Text(d["text"].as_str().unwrap_or("").into())),
                Some("thinking_delta") => out.push_back(StreamEvent::Reasoning(d["thinking"].as_str().unwrap_or("").into())),
                Some("input_json_delta") => out.push_back(StreamEvent::ToolCallDelta {
                    index: idx,
                    id: None,
                    name: None,
                    arguments: d["partial_json"].as_str().unwrap_or("").into(),
                }),
                _ => {}
            }
        }
        "message_delta" => {
            if let Some(n) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                out.push_back(StreamEvent::Usage(Usage { prompt_tokens: 0, completion_tokens: n }));
            }
            if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                out.push_back(StreamEvent::Done { finish_reason: Some(r.into()) });
            }
        }
        "message_stop" => return Ok(true),
        "error" => return Err(v.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error").into()),
        _ => {}
    }
    Ok(false)
}

enum State {
    Start(reqwest::RequestBuilder),
    Streaming { body: BoxStream<'static, reqwest::Result<bytes::Bytes>>, parser: SseParser },
    Finished,
}

impl Provider for Anthropic {
    fn id(&self) -> &str {
        "anthropic"
    }
    fn describe(&self) -> String {
        self.base_url.clone()
    }

    fn chat(&self, req: ChatRequest) -> EventStream {
        let rb = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&Self::body(&req));
        let endpoint = self.base_url.clone();
        let key_hint = self.key_hint.clone();
        futures::stream::unfold((State::Start(rb), VecDeque::new()), move |(mut state, mut pending)| {
            let endpoint = endpoint.clone();
            let key_hint = key_hint.clone();
            async move {
                loop {
                    if let Some(ev) = pending.pop_front() {
                        return Some((Ok(ev), (state, pending)));
                    }
                    match state {
                        State::Finished => return None,
                        State::Start(rb) => {
                            let resp = match rb.send().await {
                                Ok(r) => r,
                                Err(e) => {
                                    return Some((
                                        Err(ProviderError::Connect { endpoint, message: root_cause(&e) }),
                                        (State::Finished, pending),
                                    ));
                                }
                            };
                            if !resp.status().is_success() {
                                let s = resp.status().as_u16();
                                let body = resp.text().await.unwrap_or_default();
                                return Some((Err(status_error("anthropic", s, &body, &key_hint)), (State::Finished, pending)));
                            }
                            state = State::Streaming { body: resp.bytes_stream().boxed(), parser: SseParser::default() };
                        }
                        State::Streaming { mut body, mut parser } => match body.next().await {
                            Some(Ok(chunk)) => {
                                let mut stop = false;
                                for ev in parser.push(&chunk) {
                                    match parse_event(ev.event.as_deref(), &ev.data, &mut pending) {
                                        Ok(s) => stop |= s,
                                        Err(message) => {
                                            let err = ProviderError::Protocol { provider: "anthropic".into(), message };
                                            return Some((Err(err), (State::Finished, pending)));
                                        }
                                    }
                                }
                                state = if stop { State::Finished } else { State::Streaming { body, parser } };
                            }
                            Some(Err(e)) => {
                                return Some((
                                    Err(ProviderError::Connect { endpoint, message: root_cause(&e) }),
                                    (State::Finished, pending),
                                ));
                            }
                            None => state = State::Finished,
                        },
                    }
                }
            }
        })
        .boxed()
    }

    fn list_models(&self) -> futures::future::BoxFuture<'static, Result<Vec<RemoteModel>, ProviderError>> {
        let rb = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION);
        let (endpoint, key_hint) = (self.base_url.clone(), self.key_hint.clone());
        Box::pin(async move {
            let resp = rb.send().await.map_err(|e| ProviderError::Connect { endpoint, message: root_cause(&e) })?;
            let s = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if !(200..300).contains(&s) {
                return Err(status_error("anthropic", s, &text, &key_hint));
            }
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| ProviderError::Protocol { provider: "anthropic".into(), message: e.to_string() })?;
            Ok(v["data"]
                .as_array()
                .map(|a| a.iter().filter_map(|m| m["id"].as_str()).map(|id| RemoteModel { id: id.into() }).collect())
                .unwrap_or_default())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sampling;
    use common::{Message, ToolCall};

    #[test]
    fn builds_body_with_system_and_merged_tool_results() {
        let mut a = Message::assistant("");
        a.tool_calls = vec![ToolCall { id: "t1".into(), name: "ls".into(), arguments: "{\"path\":\".\"}".into() }];
        let mut r1 = Message::new(Role::Tool, "a.txt");
        r1.tool_call_id = Some("t1".into());
        let req = ChatRequest {
            model: "claude-sonnet-5-5".into(),
            messages: vec![Message::system("be brief"), Message::user("list"), a, r1],
            sampling: Sampling { temperature: Some(1.5), ..Default::default() },
            tools: vec![],
        };
        let b = Anthropic::body(&req);
        assert_eq!(b["system"], "be brief");
        assert_eq!(b["max_tokens"], 8192);
        assert_eq!(b["temperature"], 1.0);
        assert_eq!(b["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(b["messages"][1]["content"][0]["input"]["path"], ".");
        assert_eq!(b["messages"][2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn parses_stream_events() {
        let mut q = VecDeque::new();
        parse_event(Some("message_start"), r#"{"message":{"usage":{"input_tokens":12}}}"#, &mut q).unwrap();
        parse_event(Some("content_block_delta"), r#"{"index":0,"delta":{"type":"text_delta","text":"Hi"}}"#, &mut q).unwrap();
        parse_event(Some("content_block_start"), r#"{"index":1,"content_block":{"type":"tool_use","id":"x","name":"ls"}}"#, &mut q)
            .unwrap();
        parse_event(Some("content_block_delta"), r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#, &mut q).unwrap();
        parse_event(Some("message_delta"), r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#, &mut q).unwrap();
        assert!(parse_event(Some("message_stop"), "{}", &mut q).unwrap());
        assert!(parse_event(Some("error"), r#"{"error":{"message":"overloaded"}}"#, &mut q).is_err());
        assert_eq!(q.len(), 6);
        assert_eq!(q[1], StreamEvent::Text("Hi".into()));
    }
}
