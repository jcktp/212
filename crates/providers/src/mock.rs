//! Deterministic provider for tests. It replays scripted turns.

use std::sync::{Arc, Mutex};

use common::StreamEvent;
use futures::StreamExt;

use crate::{ChatRequest, EventStream, Provider, ProviderError, RemoteModel};

type Turn = Vec<Result<StreamEvent, ProviderError>>;

#[derive(Clone, Default)]
pub struct MockProvider {
    script: Arc<Mutex<Vec<Turn>>>,
    /// Every request received, for assertions.
    pub requests: Arc<Mutex<Vec<ChatRequest>>>,
    pub delay: Option<std::time::Duration>,
}

impl MockProvider {
    pub fn new(turns: Vec<Turn>) -> Self {
        Self { script: Arc::new(Mutex::new(turns.into_iter().rev().collect())), ..Default::default() }
    }
    pub fn text(turns: &[&str]) -> Self {
        Self::new(
            turns
                .iter()
                .map(|t| {
                    let mut v: Vec<_> = t.split_inclusive(' ').map(|w| Ok(StreamEvent::Text(w.to_string()))).collect();
                    v.push(Ok(StreamEvent::Done { finish_reason: Some("stop".into()) }));
                    v
                })
                .collect(),
        )
    }
}

impl Provider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }
    fn describe(&self) -> String {
        "mock provider".into()
    }
    fn chat(&self, req: ChatRequest) -> EventStream {
        self.requests.lock().unwrap().push(req);
        let turn = self.script.lock().unwrap().pop().unwrap_or_default();
        let delay = self.delay;
        futures::stream::iter(turn)
            .then(move |e| async move {
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
                e
            })
            .boxed()
    }
    fn list_models(&self) -> futures::future::BoxFuture<'static, Result<Vec<RemoteModel>, ProviderError>> {
        Box::pin(async { Ok(vec![RemoteModel { id: "mock".into() }]) })
    }
}
