//! OpenAI Chat Completions API provider (SSE streaming).
//!
//! Standard OpenAI-compatible `/chat/completions` endpoint with `stream: true`
//! and `stream_options: { include_usage: true }`. Works with any provider that
//! implements the Chat Completions API (Neuralwatt, 302ai, OpenRouter, etc.).

mod request;
mod response;

use crate::providers::{http_client, run_sse};
use crate::registry::ApiProvider;
use crate::stream::{AssistantEventStream, EventSink};
use crate::types::{Api, Context, Model, StreamOptions};

use request::{build_body, endpoint_url};
use response::Parser;

pub struct OpenAiCompletionsProvider {
    client: reqwest::Client,
}

impl OpenAiCompletionsProvider {
    pub fn new() -> Self {
        Self {
            client: http_client(),
        }
    }
}

impl Default for OpenAiCompletionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for OpenAiCompletionsProvider {
    fn api(&self) -> Api {
        Api::OpenAiCompletions
    }

    fn stream(&self, model: &Model, ctx: &Context, opts: StreamOptions) -> AssistantEventStream {
        let (sink, stream) = AssistantEventStream::channel();
        let client = self.client.clone();
        let model = model.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            run(client, model, ctx, opts, sink).await;
        });
        stream
    }
}

async fn run(
    client: reqwest::Client,
    model: Model,
    ctx: Context,
    opts: StreamOptions,
    sink: EventSink,
) {
    let body = build_body(&model, &ctx, &opts);
    let url = endpoint_url(&model);
    let mut handler = Parser::new(&model);

    let build_request = |client: &reqwest::Client| {
        let mut request = client
            .post(&url)
            .bearer_auth(opts.auth.secret())
            .header("accept", "text/event-stream");
        for (name, value) in &model.headers {
            request = request.header(name, value);
        }
        request.json(&body)
    };

    if run_sse(&client, build_request, &model, &opts, &sink, &mut handler).await {
        handler.finish(&model, &sink);
    }
}
