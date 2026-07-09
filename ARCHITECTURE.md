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
  phi_agent["phi-agent"] --> phi_ai_types["phi-ai-types"]
  phi_agent["phi-agent"] --> phi_host["phi-host"]
  phi_ai["phi-ai"] --> phi_ai_types["phi-ai-types"]
  phi_ai_auth["phi-ai-auth"] --> phi_ai_types["phi-ai-types"]
  phi_app["phi-app"] --> phi_agent["phi-agent"]
  phi_app["phi-app"] --> phi_ai["phi-ai"]
  phi_app["phi-app"] --> phi_ai_auth["phi-ai-auth"]
  phi_app["phi-app"] --> phi_ai_types["phi-ai-types"]
  phi_app["phi-app"] --> phi_host["phi-host"]
  phi_app["phi-app"] --> phi_session["phi-session"]
  phi_app["phi-app"] --> phi_tui["phi-tui"]
  phi_host["phi-host"] --> phi_ai["phi-ai"]
  phi_host["phi-host"] --> phi_ai_types["phi-ai-types"]
  phi_host["phi-host"] --> phi_session["phi-session"]
  phi_host["phi-host"] --> phi_tui["phi-tui"]
```

## Crates

| Crate | Description |
|-------|-------------|
| `phi-agent` | packages/agent port: Lua-authored agent policy pack |
| `phi-ai` | packages/ai port: transport (HTTP/SSE/retry/cancellation), protocols, registry |
| `phi-ai-auth` | packages/ai oauth port: PKCE engine, flows as data, provider registry |
| `phi-ai-types` | packages/ai type vocabulary: models, messages, content, usage, stream events |
| `phi-app` | _no description in Cargo.toml_ |
| `phi-host` | Lua extension host: coroutine async seam, event bus, watchdog |
| `phi-session` | Session manager: append-only JSONL session trees (port of core/session-manager.ts) |
| `phi-tui` | _no description in Cargo.toml_ |

## Module structure

Per-crate module/item trees (`cargo modules structure`). Functions are
omitted; types, traits, and module boundaries are the architecture.

### `phi-agent`

```

crate phi_agent
└── struct AgentEvent: pub
```

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

### `phi-ai-auth`

```

crate phi_ai_auth
├── mod anthropic: pub(crate)
├── mod callback_server: pub(crate)
│   ├── struct CallbackCode: pub
│   ├── struct CallbackPages: pub
│   └── struct CallbackServer: pub
├── mod engine: pub(crate)
│   ├── struct PkceFlow: pub
│   └── struct TokenResponse: pub(self)
├── mod error: pub(crate)
│   └── enum AuthError: pub
├── mod oauth_page: pub(crate)
│   └── struct PageOptions: pub(self)
├── mod pkce: pub(crate)
│   └── struct Pkce: pub
├── mod registry: pub(crate)
│   └── struct OAuthApiKeyResult: pub
└── mod types: pub(crate)
    ├── type AuthFuture: pub
    ├── struct OAuthAuthInfo: pub
    ├── struct OAuthCredentials: pub
    ├── struct OAuthDeviceCodeInfo: pub
    ├── trait OAuthLoginCallbacks: pub
    ├── struct OAuthPrompt: pub
    ├── type OAuthProviderId: pub
    ├── trait OAuthProviderInterface: pub
    ├── struct OAuthSelectOption: pub
    └── struct OAuthSelectPrompt: pub
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

### `phi-app`

```

crate phi_app
├── mod builtins: pub
├── mod cli: pub
│   ├── mod args: pub
│   │   ├── struct Args: pub
│   │   └── struct Diagnostic: pub
│   ├── mod list_models: pub
│   │   └── struct Row: pub(self)
│   └── mod login: pub
│       └── struct StdioLoginCallbacks: pub
├── mod config: pub
└── mod core: pub
    ├── mod auth_guidance: pub
    ├── mod auth_storage: pub
    │   ├── enum AuthCredential: pub
    │   ├── struct AuthStatus: pub
    │   ├── struct AuthStorage: pub
    │   ├── enum AuthStorageBackend: pub
    │   ├── type AuthStorageData: pub
    │   ├── enum AuthStorageError: pub
    │   └── struct FileLock: pub(self)
    ├── mod defaults: pub
    ├── mod http_dispatcher: pub
    ├── mod model_registry: pub
    │   ├── struct ModelRegistry: pub
    │   └── enum ResolvedRequestAuth: pub
    ├── mod model_resolver: pub
    │   ├── struct InitialModelResult: pub
    │   ├── struct ParsedModelResult: pub
    │   └── struct ResolveCliModelResult: pub
    ├── mod resolve_config_value: pub
    │   ├── enum ConfigValueError: pub
    │   ├── enum ConfigValueReference: pub(self)
    │   └── enum TemplatePart: pub(self)
    └── mod settings_manager: pub
        ├── struct BranchSummarySettings: pub
        ├── struct CompactionSettings: pub
        ├── struct FileLock: pub(self)
        ├── struct ProviderRetrySettings: pub
        ├── struct RetrySettings: pub
        ├── type Settings: pub
        ├── struct SettingsError: pub
        ├── struct SettingsManager: pub
        ├── struct SettingsManagerCreateOptions: pub
        ├── enum SettingsManagerError: pub
        ├── enum SettingsScope: pub
        └── enum SettingsStorage: pub
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
├── type ToolUpdateCallback: pub
├── mod ai: pub(crate)
│   └── struct LuaAbortSignal: pub(crate)
├── mod api: pub(crate)
│   ├── struct LuaEditor: pub(self)
│   ├── struct LuaSelectList: pub(self)
│   └── struct ResolvedCommand: pub(crate)
├── mod convert: pub(crate)
├── mod discover: pub
├── mod error: pub(crate)
│   └── enum HostError: pub
├── mod exec: pub(crate)
│   └── struct ExecResult: pub(crate)
├── mod os: pub(crate)
├── mod paths: pub(crate)
├── mod schema: pub(crate)
│   └── struct SchemaError: pub(crate)
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
    ├── struct ToolInvocation: pub(self)
    ├── struct WatchdogState: pub(self)
    └── struct Watched: pub(self)
```

### `phi-session`

```

crate phi_session
├── mod messages: pub
├── mod paths: pub
│   └── struct PathInputOptions: pub
├── mod session_manager: pub
│   ├── type FileEntry: pub
│   ├── enum Leaf: pub
│   ├── struct NewSessionOptions: pub
│   ├── type Result: pub(self)
│   ├── struct SessionContext: pub
│   ├── enum SessionError: pub
│   ├── struct SessionInfo: pub
│   ├── type SessionListProgress: pub
│   ├── struct SessionManager: pub
│   ├── struct SessionModel: pub
│   └── struct SessionTreeNode: pub
├── mod time: pub
└── mod uuid: pub
    └── struct State: pub(self)
```

### `phi-tui`

```

crate phi_tui
├── mod component: pub
│   ├── trait Component: pub
│   ├── struct Container: pub
│   ├── struct Renderer: pub
│   └── struct Text: pub
├── mod editor: pub
│   ├── enum Action: pub(self)
│   ├── struct Editor: pub
│   └── struct State: pub(self)
├── mod fuzzy: pub
│   └── struct FuzzyMatch: pub
├── mod kill_ring: pub
│   └── struct KillRing: pub
├── mod select_list: pub
│   ├── struct SelectItem: pub
│   └── struct SelectList: pub
└── mod undo_stack: pub
    └── struct UndoStack: pub
```
