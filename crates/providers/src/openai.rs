//! OpenAI Chat Completions streaming. Used for the built-in llama.cpp server,
//! Ollama (`/v1`), OpenAI, and any OpenAI-compatible endpoint.

use std::collections::VecDeque;
use std::time::Duration;

use common::{Message, Role, StreamEvent, Usage};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};

use crate::sse::SseParser;
use crate::{ChatRequest, EventStream, Provider, ProviderError, RemoteModel, status_error};

#[derive(Clone)]
pub struct OpenAiCompatible {
    id: String,
    base_url: String,
    api_key: Option<String>,
    key_hint: String,
    client: reqwest::Client,
    /// OpenAI's own API wants `max_completion_tokens`; most servers want `max_tokens`.
    official_openai: bool,
}

impl std::fmt::Debug for OpenAiCompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key.
        f.debug_struct("OpenAiCompatible").field("id", &self.id).field("base_url", &self.base_url).finish()
    }
}

impl OpenAiCompatible {
    /// `base_url` is the API root including `/v1`.
    pub fn new(id: &str, base_url: &str, api_key: Option<String>, key_hint: &str) -> Self {
        let loopback = base_url.contains("://127.0.0.1") || base_url.contains("://localhost") || base_url.contains("://[::1]");
        let mut b = {
            crate::init_tls();
            reqwest::Client::builder()
        }
        .user_agent(concat!("212/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10));
        if loopback {
            b = b.no_proxy();
        }
        Self {
            id: id.into(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            key_hint: key_hint.into(),
            client: b.build().expect("client"),
            official_openai: base_url.contains("api.openai.com"),
        }
    }

    pub fn body(&self, req: &ChatRequest) -> Value {
        let messages: Vec<Value> = req.messages.iter().map(message_json).collect();
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        let o = body.as_object_mut().unwrap();
        if let Some(t) = req.sampling.temperature {
            o.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.sampling.top_p {
            o.insert("top_p".into(), json!(p));
        }
        if let Some(m) = req.sampling.max_tokens {
            o.insert(if self.official_openai { "max_completion_tokens" } else { "max_tokens" }.into(), json!(m));
        }
        if !req.tools.is_empty() {
            o.insert("tools".into(), Value::Array(req.tools.clone()));
        }
        body
    }
}

fn message_json(m: &Message) -> Value {
    let mut v = json!({"role": m.role.as_str(), "content": m.content});
    if m.role == Role::Assistant && !m.tool_calls.is_empty() {
        v["tool_calls"] = m
            .tool_calls
            .iter()
            .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments}}))
            .collect();
    }
    if let Some(id) = &m.tool_call_id {
        v["tool_call_id"] = json!(id);
    }
    v
}

/// Translate one `data:` payload into events.
pub fn parse_chunk(data: &str, out: &mut VecDeque<StreamEvent>) -> Result<(), String> {
    let v: Value = serde_json::from_str(data).map_err(|e| format!("{e}: {}", data.chars().take(120).collect::<String>()))?;
    if let Some(err) = v.get("error") {
        return Err(err.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_string());
    }
    if let Some(choice) = v.get("choices").and_then(|c| c.get(0)) {
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = delta.get(key).and_then(Value::as_str)
                && !r.is_empty()
            {
                out.push_back(StreamEvent::Reasoning(r.to_string()));
            }
        }
        if let Some(t) = delta.get("content").and_then(Value::as_str)
            && !t.is_empty()
        {
            out.push_back(StreamEvent::Text(t.to_string()));
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (i, c) in calls.iter().enumerate() {
                out.push_back(StreamEvent::ToolCallDelta {
                    index: c.get("index").and_then(Value::as_u64).map_or(i, |n| n as usize),
                    id: c.get("id").and_then(Value::as_str).map(str::to_string),
                    name: c.pointer("/function/name").and_then(Value::as_str).map(str::to_string),
                    arguments: c.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("").to_string(),
                });
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            out.push_back(StreamEvent::Done { finish_reason: Some(reason.to_string()) });
        }
    }
    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        out.push_back(StreamEvent::Usage(Usage {
            prompt_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            completion_tokens: u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
        }));
    }
    // llama.cpp reports measured speed.
    if let Some(tps) = v.pointer("/timings/predicted_per_second").and_then(Value::as_f64) {
        out.push_back(StreamEvent::Speed { tokens_per_second: tps });
    }
    Ok(())
}

enum State {
    Start(reqwest::RequestBuilder),
    Streaming { body: BoxStream<'static, reqwest::Result<bytes::Bytes>>, parser: SseParser },
    Finished,
}

impl Provider for OpenAiCompatible {
    fn id(&self) -> &str {
        &self.id
    }

    fn describe(&self) -> String {
        self.base_url.clone()
    }

    fn chat(&self, req: ChatRequest) -> EventStream {
        let mut rb = self.client.post(format!("{}/chat/completions", self.base_url)).json(&self.body(&req));
        if let Some(k) = &self.api_key {
            rb = rb.bearer_auth(k);
        }
        let provider = self.id.clone();
        let endpoint = self.base_url.clone();
        let key_hint = self.key_hint.clone();
        let init = (State::Start(rb), VecDeque::<StreamEvent>::new());
        futures::stream::unfold(init, move |(mut state, mut pending)| {
            let provider = provider.clone();
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
                                    let err = ProviderError::Connect { endpoint, message: root_cause(&e) };
                                    return Some((Err(err), (State::Finished, pending)));
                                }
                            };
                            let status = resp.status();
                            if !status.is_success() {
                                let body = resp.text().await.unwrap_or_default();
                                let err = status_error(&provider, status.as_u16(), &body, &key_hint);
                                return Some((Err(err), (State::Finished, pending)));
                            }
                            state = State::Streaming { body: resp.bytes_stream().boxed(), parser: SseParser::default() };
                        }
                        State::Streaming { mut body, mut parser } => match body.next().await {
                            Some(Ok(chunk)) => {
                                for ev in parser.push(&chunk) {
                                    if ev.data == "[DONE]" {
                                        continue;
                                    }
                                    if let Err(message) = parse_chunk(&ev.data, &mut pending) {
                                        let err = ProviderError::Protocol { provider: provider.clone(), message };
                                        return Some((Err(err), (State::Finished, pending)));
                                    }
                                }
                                state = State::Streaming { body, parser };
                            }
                            Some(Err(e)) => {
                                let err = ProviderError::Connect { endpoint, message: root_cause(&e) };
                                return Some((Err(err), (State::Finished, pending)));
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
        let mut rb = self.client.get(format!("{}/models", self.base_url));
        if let Some(k) = &self.api_key {
            rb = rb.bearer_auth(k);
        }
        let (provider, endpoint, key_hint) = (self.id.clone(), self.base_url.clone(), self.key_hint.clone());
        Box::pin(async move {
            let resp = rb.send().await.map_err(|e| ProviderError::Connect { endpoint, message: root_cause(&e) })?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(status_error(&provider, status.as_u16(), &text, &key_hint));
            }
            let v: Value =
                serde_json::from_str(&text).map_err(|e| ProviderError::Protocol { provider: provider.clone(), message: e.to_string() })?;
            Ok(v.get("data")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|m| m.get("id")?.as_str()).map(|id| RemoteModel { id: id.into() }).collect())
                .unwrap_or_default())
        })
    }
}

pub(crate) fn root_cause(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        msg = s.to_string();
        src = s.source();
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sampling;

    #[test]
    fn parses_content_reasoning_tools_usage_and_speed() {
        let mut q = VecDeque::new();
        parse_chunk(r#"{"choices":[{"delta":{"reasoning_content":"hmm"}}]}"#, &mut q).unwrap();
        parse_chunk(r#"{"choices":[{"delta":{"content":"Hi"}}]}"#, &mut q).unwrap();
        parse_chunk(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#,
            &mut q,
        )
        .unwrap();
        parse_chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":7},"timings":{"predicted_per_second":42.5}}"#, &mut q).unwrap();
        let v: Vec<_> = q.into_iter().collect();
        assert_eq!(v[0], StreamEvent::Reasoning("hmm".into()));
        assert_eq!(v[1], StreamEvent::Text("Hi".into()));
        assert!(
            matches!(&v[2], StreamEvent::ToolCallDelta { index: 0, id: Some(id), name: Some(n), arguments } if id == "c1" && n == "read_file" && arguments == "{\"pa")
        );
        assert_eq!(v[3], StreamEvent::Done { finish_reason: Some("stop".into()) });
        assert_eq!(v[4], StreamEvent::Usage(Usage { prompt_tokens: 5, completion_tokens: 7 }));
        assert_eq!(v[5], StreamEvent::Speed { tokens_per_second: 42.5 });
    }

    #[test]
    fn errors_in_stream_are_reported() {
        let mut q = VecDeque::new();
        assert_eq!(parse_chunk(r#"{"error":{"message":"context full"}}"#, &mut q).unwrap_err(), "context full");
        assert!(parse_chunk("not json", &mut q).is_err());
    }

    #[test]
    fn body_uses_correct_token_field_and_never_leaks_key() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![Message::system("s"), Message::user("u")],
            sampling: Sampling { temperature: Some(0.5), top_p: None, max_tokens: Some(10) },
            tools: vec![],
        };
        let local = OpenAiCompatible::new("local", "http://127.0.0.1:1/v1", Some("secret".into()), "");
        let b = local.body(&req);
        assert_eq!(b["max_tokens"], 10);
        assert_eq!(b["messages"][1]["role"], "user");
        assert!(!b.to_string().contains("secret"));
        assert!(!format!("{local:?}").contains("secret"));
        let oai = OpenAiCompatible::new("openai", "https://api.openai.com/v1", None, "");
        assert_eq!(oai.body(&req)["max_completion_tokens"], 10);
    }

    #[tokio::test]
    async fn streams_from_a_real_http_server_and_reports_connect_errors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0; 8192];
            let _ = s.read(&mut buf).await;
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let head = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n", body.len());
            s.write_all(head.as_bytes()).await.unwrap();
            s.write_all(body.as_bytes()).await.unwrap();
        });
        let p = OpenAiCompatible::new("local", &format!("http://{addr}/v1"), Some("k".into()), "");
        let req = ChatRequest { model: "m".into(), messages: vec![Message::user("hi")], sampling: Sampling::default(), tools: vec![] };
        let events: Vec<_> = p.chat(req.clone()).collect().await;
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                Ok(StreamEvent::Text(t)) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello");

        let dead = OpenAiCompatible::new("local", "http://127.0.0.1:1/v1", None, "");
        let events: Vec<_> = dead.chat(req).collect().await;
        assert!(matches!(events.as_slice(), [Err(ProviderError::Connect { .. })]));
    }
}
