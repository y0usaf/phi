//! Anthropic Messages API provider (SSE streaming).

mod request;
mod response;

use crate::providers::{http_client, run_sse};
use crate::registry::ApiProvider;
use crate::stream::{AssistantEventStream, EventSink};
use crate::types::{Api, Auth, Context, Model, StreamOptions};

use request::{ANTHROPIC_VERSION, CLAUDE_CLI_VERSION, OAUTH_BETA, build_body};
use response::Parser;

pub struct AnthropicProvider {
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new() -> Self {
        Self {
            client: http_client().expect("reqwest client must build"),
        }
    }
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for AnthropicProvider {
    fn api(&self) -> Api {
        Api::AnthropicMessages
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
    let url = format!("{}/v1/messages", model.base_url.trim_end_matches('/'));
    let mut handler = Parser::new(&model);

    let build_request = |client: &reqwest::Client| {
        let mut request = client
            .post(&url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("accept", "application/json");
        request = match &opts.auth {
            Auth::OAuthToken(token) => request
                .bearer_auth(token)
                .header("anthropic-beta", OAUTH_BETA)
                .header("user-agent", format!("claude-cli/{CLAUDE_CLI_VERSION}"))
                .header("x-app", "cli"),
            Auth::ApiKey(key) => request.header("x-api-key", key),
            Auth::ChatGptOAuth { access, .. } => request.bearer_auth(access),
        };
        for (name, value) in &model.headers {
            request = request.header(name, value);
        }
        request.json(&body)
    };

    if run_sse(&client, build_request, &model, &opts, &sink, &mut handler).await {
        handler.finish(&model, &sink);
    }
}
