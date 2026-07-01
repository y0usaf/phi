//! OpenAI Responses API provider (SSE streaming).

mod request;
mod response;

use crate::providers::{http_client, run_sse};
use crate::registry::ApiProvider;
use crate::stream::AssistantEventStream;
use crate::types::{Api, Auth, Context, Model, StreamOptions};

use request::build_body;
use response::Parser;

pub struct OpenAiResponsesProvider {
    client: reqwest::Client,
}

impl OpenAiResponsesProvider {
    pub fn new() -> Self {
        Self {
            client: http_client(),
        }
    }
}

impl Default for OpenAiResponsesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for OpenAiResponsesProvider {
    fn api(&self) -> Api {
        Api::OpenAiResponses
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
    sink: crate::stream::EventSink,
) {
    let body = build_body(&model, &ctx, &opts);
    let base_url = match &opts.auth {
        Auth::ChatGptOAuth { .. } => request::CODEX_BASE_URL,
        _ => model.base_url.trim_end_matches('/'),
    };
    let url = format!("{base_url}/responses");
    let mut handler = Parser::new(&model);

    let build_request = |client: &reqwest::Client| {
        let mut request = client
            .post(&url)
            .bearer_auth(opts.auth.secret())
            .header("accept", "text/event-stream");
        if let Auth::ChatGptOAuth { account_id, .. } = &opts.auth {
            request = request
                .header("chatgpt-account-id", account_id)
                .header("originator", "phi");
        }
        for (name, value) in &model.headers {
            request = request.header(name, value);
        }
        request.json(&body)
    };

    if run_sse(&client, build_request, &model, &opts, &sink, &mut handler).await {
        handler.finish(&model, &sink);
    }
}
