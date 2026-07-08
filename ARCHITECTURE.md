<!-- GENERATED FILE — do not edit by hand.
     Regenerate: scripts/gen-arch.sh
     Freshness is enforced by `nix flake check` (checks.arch-fresh). -->

# Architecture map (generated)

Structural map of the workspace, extracted from the code. For rationale,
invariants, and dataflow, see DESIGN.md.

## Crate dependency graph

Internal (workspace-local) dependencies only. `A --> B` means A depends on B.

```mermaid
graph TD
  phi_ai["phi-ai"] --> phi_ai_types["phi-ai-types"]
```

## Crates

| Crate | Description |
|-------|-------------|
| `phi-ai` | packages/ai port: transport (HTTP/SSE/retry/cancellation), protocols, registry |
| `phi-ai-types` | packages/ai type vocabulary: models, messages, content, usage, stream events |
| `phi-host` | Lua extension host: coroutine async seam, event bus, watchdog |

## Module structure

Per-crate module/item trees (`cargo modules structure`). Functions are
omitted; types, traits, and module boundaries are the architecture.

### `phi-ai`

```

crate phi_ai
├── mod protocols: pub
│   ├── struct ProtocolError: pub
│   ├── mod anthropic: pub
│   │   ├── struct AnthropicOptions: pub
│   │   ├── enum AnthropicThinkingDisplay: pub
│   │   ├── enum AnthropicToolChoice: pub
│   │   ├── struct BlockMeta: pub(self)
│   │   ├── struct PreparedRequest: pub(self)
│   │   └── struct ResolvedCompat: pub(self)
│   ├── mod cloudflare: pub
│   ├── mod copilot_headers: pub
│   ├── mod openai_completions: pub
│   │   ├── struct OpenAICompletionsOptions: pub
│   │   ├── enum OpenAIToolChoice: pub
│   │   ├── struct PreparedRequest: pub(self)
│   │   ├── struct ResolvedCompat: pub(self)
│   │   └── struct ToolMeta: pub(self)
│   ├── mod openai_prompt_cache: pub
│   ├── mod options: pub
│   │   ├── type PayloadHook: pub
│   │   ├── type ResponseHook: pub
│   │   ├── struct SimpleStreamOptions: pub
│   │   └── struct StreamOptions: pub
│   ├── mod simple_options: pub
│   │   └── struct AdjustedTokens: pub
│   └── mod transform_messages: pub
│       └── type NormalizeToolCallId: pub
├── mod registry: pub
│   ├── mod api_registry: pub
│   │   ├── struct ApiProvider: pub
│   │   ├── type ApiStreamFn: pub
│   │   ├── type ApiStreamSimpleFn: pub
│   │   └── struct Registered: pub(self)
│   ├── mod catalog: pub
│   │   └── struct ProviderModels: pub(self)
│   ├── mod env_api_keys: pub
│   └── mod stream: pub
├── mod transport: pub
│   ├── enum TransportError: pub
│   ├── mod abort: pub
│   │   └── struct AbortSignal: pub
│   ├── mod event_stream: pub
│   │   ├── type AssistantMessageEventStream: pub
│   │   ├── type CompleteFn: pub(self)
│   │   ├── struct EventStream: pub
│   │   ├── enum ResultSlot: pub(self)
│   │   └── struct State: pub(self)
│   ├── mod http: pub
│   │   └── struct RetryOptions: pub
│   ├── mod retry: pub
│   └── mod sse: pub
│       ├── struct SseDecoder: pub
│       ├── struct SseEvent: pub
│       └── struct SseReader: pub
└── mod util: pub
    ├── mod headers: pub
    ├── mod json_parse: pub
    │   ├── struct Parser: pub(self)
    │   └── enum PartialJsonError: pub
    └── mod sanitize: pub
```

### `phi-ai-types`

```

crate phi_ai_types
├── mod diagnostics: pub
│   ├── struct AssistantMessageDiagnostic: pub
│   ├── enum DiagnosticCode: pub
│   └── struct DiagnosticErrorInfo: pub
├── mod models: pub
└── mod types: pub
    ├── struct AnthropicMessagesCompat: pub
    ├── type Api: pub
    ├── enum AssistantContent: pub
    ├── struct AssistantImages: pub
    ├── struct AssistantMessage: pub
    ├── enum AssistantMessageEvent: pub
    ├── enum AssistantRole: pub
    ├── enum CacheControlFormat: pub
    ├── enum CacheRetention: pub
    ├── struct Context: pub
    ├── enum DataCollection: pub
    ├── struct ImageContent: pub
    ├── enum ImageType: pub
    ├── type ImagesApi: pub
    ├── struct ImagesContext: pub
    ├── struct ImagesModel: pub
    ├── type ImagesProvider: pub
    ├── enum ImagesStopReason: pub
    ├── enum MaxTokensField: pub
    ├── enum Message: pub
    ├── enum Modality: pub
    ├── struct Model: pub
    ├── struct ModelCost: pub
    ├── enum ModelThinkingLevel: pub
    ├── struct OpenAICompletionsCompat: pub
    ├── struct OpenAIResponsesCompat: pub
    ├── struct OpenRouterRouting: pub
    ├── type Provider: pub
    ├── struct ProviderResponse: pub
    ├── enum StopReason: pub
    ├── struct TextContent: pub
    ├── enum TextOrImageContent: pub
    ├── enum TextSignaturePhase: pub
    ├── struct TextSignatureV1: pub
    ├── enum TextType: pub
    ├── struct ThinkingBudgets: pub
    ├── struct ThinkingContent: pub
    ├── enum ThinkingFormat: pub
    ├── enum ThinkingLevel: pub
    ├── type ThinkingLevelMap: pub
    ├── enum ThinkingType: pub
    ├── struct Tool: pub
    ├── struct ToolCall: pub
    ├── enum ToolCallType: pub
    ├── struct ToolResultMessage: pub
    ├── enum ToolResultRole: pub
    ├── enum Transport: pub
    ├── struct Usage: pub
    ├── struct UsageCost: pub
    ├── enum UserContent: pub
    ├── struct UserMessage: pub
    ├── enum UserRole: pub
    └── struct VercelGatewayRouting: pub
```

### `phi-host`

```

crate phi_host
├── struct CommandInfo: pub
├── struct EmbeddedPack: pub
├── struct Host: pub
├── struct HostConfig: pub
├── struct LoadError: pub
├── struct LoadReport: pub
├── struct Outcome: pub
├── struct ProviderInfo: pub
├── struct ToolInfo: pub
├── mod api: pub(crate)
│   └── struct ResolvedCommand: pub(crate)
├── mod convert: pub(crate)
├── mod discover: pub
├── mod error: pub(crate)
│   └── enum HostError: pub
├── mod exec: pub(crate)
│   └── struct ExecResult: pub(crate)
├── mod os: pub(crate)
├── mod paths: pub(crate)
├── mod trust: pub
│   ├── struct ProjectTrustStore: pub
│   ├── struct ResolveProjectTrust: pub
│   ├── struct TrustEntry: pub
│   ├── enum TrustError: pub
│   ├── struct TrustEventResult: pub
│   ├── type TrustFile: pub(self)
│   ├── struct TrustLock: pub(self)
│   ├── struct TrustOption: pub
│   ├── enum TrustResolution: pub
│   └── struct TrustUpdate: pub
└── mod vm: pub(crate)
    ├── enum Msg: pub(crate)
    ├── struct WatchdogState: pub(self)
    └── struct Watched: pub(self)
```
