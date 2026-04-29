# Plan: Add OpenAI Codex OAuth Login for Zed Agent

## Purpose

Add OpenAI Codex / ChatGPT Plus-Pro OAuth login as an additional Zed Agent language model provider, while preserving the existing OpenAI API-key provider behavior. This lets users authenticate with a ChatGPT subscription for Codex models without conflating that flow with OpenAI Platform API-key billing.

This document is an implementation plan only. Do not implement changes while following this plan unless explicitly asked to execute it.

## Summary of Decisions

- Add a **separate provider** for ChatGPT/Codex OAuth rather than extending the existing `OpenAI` API-key provider in-place.
- Keep existing OpenAI API-key support unchanged.
- **Reuse and lift existing OAuth helpers** from `crates/context_server/src/oauth.rs` (PKCE, callback parsing, redacted-`Debug` token types) into a shared location instead of reimplementing them.
- **Refactor `stream_response`** to split request building from SSE parsing, so the Codex code path can reuse the parser without duplicating event-type handling.
- Use a Codex-local **string-based model allow-list** rather than extending `open_ai::Model` for the first cut, to avoid destabilizing the existing OpenAI provider.
- Add a new OAuth credential state because `ApiKeyState` only supports opaque API keys and does not model refresh tokens, expiration, or ChatGPT account IDs.
- Use `future::Shared<Task<...>>` (the same pattern as `ApiKeyState::load_task`) to dedupe concurrent token refreshes.
- Use existing Zed internal logging (`log::*`). Do **not** add a new telemetry system.
- Do not log tokens, auth codes, JWTs, or full callback URLs. Enforce this structurally via redacted `Debug` impls, not by grepping.
- Use `LanguageModelCompletionError::AuthenticationError`, not `NoApiKey`, when the user is not signed in to OAuth.

## Goals

- [ ] Users can select an OpenAI Codex / ChatGPT subscription provider in Zed Agent.
- [ ] Users can sign in via OpenAI OAuth using browser-based PKCE authorization-code flow.
- [ ] Sign-in works on remote/SSH sessions when port 1455 is forwarded.
- [ ] Zed stores access token, refresh token, expiry, and ChatGPT account ID securely through the existing `CredentialsProvider` abstraction.
- [ ] Zed refreshes expired/near-expired Codex access tokens before requests, deduping concurrent refreshes.
- [ ] Codex model requests are routed to `https://chatgpt.com/backend-api/codex/responses`.
- [ ] Existing OpenAI API-key provider continues to work exactly as before.
- [ ] Configuration UI clearly distinguishes OpenAI Platform API-key auth from ChatGPT/Codex OAuth auth, with cross-links between the two providers' setup screens.

## Non-Goals

- [ ] Do not introduce a new telemetry/logging backend or exporter.
- [ ] Do not replace or migrate existing OpenAI API-key credentials.
- [ ] Do not import `~/.codex/auth.json` in the first implementation unless explicitly requested.
- [ ] Do not implement device-code/headless login in the first implementation unless browser flow proves unsuitable.
- [ ] Do not change global model selection semantics except registering the new provider.
- [ ] Do not extend the `open_ai::Model` enum with new Codex variants in this PR; use a Codex-local allow-list instead.

## Internal Code References

### Existing OpenAI Provider

- `crates/language_models/src/provider/open_ai.rs`
  - Existing `OpenAiLanguageModelProvider`.
  - Existing API-key `State` using `ApiKeyState`.
  - Existing `ConfigurationView` for OpenAI API keys.
  - Existing stream request paths for chat completions and Responses API.

- `crates/language_models/src/provider.rs`
  - Add module export for the new provider.

- `crates/language_models/src/language_models.rs`
  - Register the new provider in `register_language_model_providers`.

### Existing OAuth Helpers (to reuse, not reimplement)

- `crates/context_server/src/oauth.rs`
  - `generate_pkce_challenge()` — RFC 7636 S256 PKCE generator.
  - `OAuthCallback::parse_query()` — parses `code`/`state`/`error`/`error_description`, has redacted `Debug`.
  - `start_callback_server()` — `tiny_http` loopback server with `recv_timeout` cancellation, `Keep-Alive: 0` to free sockets, `CALLBACK_TIMEOUT`.
  - `validate_oauth_url()` / `require_https_or_loopback()` — SSRF/IP-range checks.
  - `OAuthTokens`, `TokenResponse`, `PkceChallenge` — all with redacted `Debug` impls. Reference for our new credential structs.
  - `McpOAuthTokenProvider` — refresh-capable token provider with `expires_at` window. Reference for the refresh logic, but **note** that it does not dedupe concurrent refreshes — we need the `future::Shared` pattern below.

These helpers are MCP-specific in their public API today (they assume DCR/CIMD discovery). Milestone 0 below promotes the truly-generic pieces (PKCE, callback parsing, callback server, redacted token types) into a shared crate so both MCP and Codex use the same code.

### Credential and Provider Abstractions

- `crates/credentials_provider/src/credentials_provider.rs`
  - Existing secure persistence abstraction:
    - `read_credentials`
    - `write_credentials`
    - `delete_credentials`

- `crates/language_model/src/api_key.rs`
  - Existing `ApiKeyState` design for API-key-only providers.
  - **Critical reference for refresh-stampede dedup:** `ApiKeyState::load_task: Option<future::Shared<Task<()>>>`. We use the same pattern to ensure only one in-flight token refresh runs at a time.

- `crates/language_model/src/language_model.rs`
  - `LanguageModelProvider` trait (full surface required for compilation, even on the first compilable check-in — see Milestone 1+2).
  - `LanguageModelProviderState` trait.
  - `ConfigurationViewTargetAgent`.

- `crates/language_model_core/src/language_model_core.rs`
  - `LanguageModelCompletionError`. Use `AuthenticationError { provider, message }` for OAuth-not-signed-in. Do not use `NoApiKey`.

- `crates/language_model_core/src/provider.rs`
  - Existing provider IDs and provider names.
  - The new provider can use `LanguageModelProviderId::new("openai_codex")` locally rather than adding a core constant unless project convention prefers core constants for all built-ins.

### OpenAI Request and Stream Mapping

- `crates/open_ai/src/open_ai.rs`
  - Existing model enum includes several Codex-capable model IDs (`gpt-5-codex`, `gpt-5.2`, `gpt-5.2-codex`, `gpt-5.3-codex`, `gpt-5.4`, …). The new provider does **not** depend on these enum variants — see "Model Allow-List" below.

- `crates/open_ai/src/responses.rs`
  - Existing `Request` type for standard OpenAI Responses API.
  - Existing `stream_response` helper posts to `{api_url}/responses` using API-key auth.
  - Existing `StreamEvent` taxonomy already includes events needed for Codex streams.
  - **Will be refactored** in Milestone 5b to split request-building from SSE parsing.

- `crates/open_ai/src/completion.rs`
  - Existing `into_open_ai_response` request conversion (Codex conversion will be a sibling that reuses the same input-item mapping).
  - Existing `OpenAiResponseEventMapper` used by the current OpenAI provider — reused as-is for Codex.

### Header-Injecting HTTP Client Pattern

- `crates/language_models/src/provider/opencode.rs`
  - `InjectHeaderClient` wraps `Arc<dyn HttpClient>` and adds a single header per request. Codex needs three custom headers (`Authorization`, `ChatGPT-Account-ID`, `originator`), so we either:
    - Build the request directly with all headers (preferred — simpler, see Milestone 6), or
    - Generalize `InjectHeaderClient` to accept multiple headers and lift it to `crates/http_client/`. Optional follow-up.

### Browser OAuth / Callback Patterns in Zed

- `crates/client/src/client.rs`
  - `authenticate_with_browser` demonstrates browser opening via `cx.open_url` and ephemeral-port callback. The Codex flow uses the **fixed** port 1455, not ephemeral.

### Configuration UI Patterns

- `crates/language_models/src/provider/open_ai.rs` — existing API-key configuration UI.
- `crates/copilot_ui/src/sign_in.rs` — existing auth UI patterns for sign-in/signing-in/signed-in/error/sign-out states.
- `crates/ui/src/components/ai/configured_api_card.rs` — existing configured-credential card.

### Cargo Manifests

- `crates/language_models/Cargo.toml`
  - Add provider-local dependencies for OAuth/PKCE/callback parsing if they aren't already pulled in transitively.
- `Cargo.toml`
  - Workspace already includes the dependency versions we need:
    - `base64 = "0.22"`, `rand = "0.9"`, `serde_urlencoded = "0.7"`, `sha2 = "0.10"`, `tiny_http = "0.12"`, `url = "2.2"`, `urlencoding = "2.1.2"`.

## External Reference Implementations

| Reference | Git URL | Relevant local path | Notes |
| --- | --- | --- | --- |
| Shuvgeist Codex OAuth | `git@github.com:shuv1337/shuvgeist.git` | `/home/shuv/repos/shuvgeist/src/oauth/openai-codex.ts` | Clean browser-extension PKCE flow, token refresh, account ID extraction. Uses minimal scopes (`openid profile email offline_access`). |
| Opencode Codex plugin | `git@github.com:anomalyco/opencode.git` | `/home/shuv/repos/opencode/packages/opencode/src/plugin/codex.ts` | Browser auth with fixed port, model filtering by string match, required headers, omits `max_output_tokens`. Uses minimal scopes. |
| OpenAI Codex CLI | `git@github.com:openai/codex.git` | `/home/shuv/repos/codex-cli/codex-rs/login/src/server.rs` | Canonical local callback server, `bind_server()` with `MAX_ATTEMPTS=10`/`RETRY_DELAY=200ms` and `send_cancel_request(port)` for port-in-use handling. Auth URL parameters. Includes broader `api.connectors.*` scopes. |
| OpenAI Codex CLI request API | `git@github.com:openai/codex.git` | `/home/shuv/repos/codex-cli/codex-rs/codex-api/src/common.rs` | `ResponsesApiRequest` includes `instructions`, `store`, `stream`, `prompt_cache_key`, etc. |
| OpenAI Codex CLI auth headers | `git@github.com:openai/codex.git` | `/home/shuv/repos/codex-cli/codex-rs/model-provider/src/bearer_auth_provider.rs` | Adds `Authorization` and `ChatGPT-Account-ID` headers. |
| OpenAI Codex CLI default client | `git@github.com:openai/codex.git` | `/home/shuv/repos/codex-cli/codex-rs/login/src/auth/default_client.rs` | `originator` handling. **First-party allow-list** is `codex_cli_rs`, `codex-tui`, `codex_vscode`, `Codex *`. `originator: "zed"` is *not* on this list — see Risks. |
| OpenAI Codex CLI Cloudflare cookies | `git@github.com:openai/codex.git` | `/home/shuv/repos/codex-cli/codex-rs/codex-client/src/chatgpt_cloudflare_cookies.rs` | Optional/advanced handling if Codex backend requires Cloudflare cookie persistence. |

## Technical Specification

### Provider Identity

Use a distinct provider identity to avoid confusing OpenAI Platform API-key auth with ChatGPT subscription OAuth.

```rust
const PROVIDER_ID: LanguageModelProviderId = LanguageModelProviderId::new("openai_codex");
const PROVIDER_NAME: LanguageModelProviderName = LanguageModelProviderName::new("OpenAI Codex");
```

Expected model IDs in selectors will become:

```text
openai_codex/gpt-5-codex
openai_codex/gpt-5.2
openai_codex/gpt-5.2-codex
openai_codex/gpt-5.3-codex
openai_codex/gpt-5.4
```

The provider ID becomes part of user settings (e.g. `default_model: "openai_codex/gpt-5.3-codex"`), so it is effectively public API. **Do not rename it after release** without a settings migration. If product wants a different display/provider ID, decide before implementation.

### Settings Schema (decide upfront)

The provider gets an `OpenAiCodexSettings` block in `AllLanguageModelSettings`. First-cut fields:

```rust
#[derive(Default, Clone, Debug, PartialEq)]
pub struct OpenAiCodexSettings {
    /// Optional override of `https://chatgpt.com/backend-api/codex` (testing/staging only).
    pub api_url: String,
    /// Optional override of `https://auth.openai.com` (testing/staging only).
    pub auth_url: String,
}
```

No `available_models` block in v1 — Codex models come exclusively from the allow-list below. Add `available_models` later if users need to register custom Codex models.

### Model Allow-List

Use a string-based allow-list rather than `open_ai::Model` enum variants. This keeps the change scoped to the new provider and avoids destabilizing existing OpenAI tests. Initial set:

```rust
const CODEX_MODELS: &[CodexModel] = &[
    CodexModel { id: "gpt-5-codex", display_name: "GPT-5 Codex", max_tokens: 272_000, max_output: 128_000, supports_thinking: false },
    CodexModel { id: "gpt-5.2", display_name: "GPT-5.2", max_tokens: 400_000, max_output: 128_000, supports_thinking: false },
    CodexModel { id: "gpt-5.2-codex", display_name: "GPT-5.2 Codex", max_tokens: 400_000, max_output: 128_000, supports_thinking: false },
    CodexModel { id: "gpt-5.3-codex", display_name: "GPT-5.3 Codex", max_tokens: 400_000, max_output: 128_000, supports_thinking: true },
    CodexModel { id: "gpt-5.4", display_name: "GPT-5.4", max_tokens: 1_050_000, max_output: 128_000, supports_thinking: false },
];
```

Token-count constants must match `crates/open_ai/src/open_ai.rs:218-258` for the corresponding variants. If they drift, add a debug-only assertion that pulls from the enum at provider startup.

### OAuth Constants

| Value | Setting |
| --- | --- |
| Client ID | `app_EMoamEEZ73f0CkXaXp7hrann` |
| Issuer | `https://auth.openai.com` |
| Authorization URL | `https://auth.openai.com/oauth/authorize` |
| Token URL | `https://auth.openai.com/oauth/token` |
| Redirect URI | `http://localhost:1455/auth/callback` |
| Scope | `openid profile email offline_access` |
| API base | `https://chatgpt.com/backend-api/codex` |
| Responses endpoint | `https://chatgpt.com/backend-api/codex/responses` |
| Originator | `zed` |

**Scope rationale:** Codex CLI requests broader `openid profile email offline_access api.connectors.read api.connectors.invoke` because it wires up MCP-style connectors. Zed v1 doesn't, so we ship the minimal scope set (matching `opencode` and `shuvgeist`). If/when Zed adds Codex-side connectors, expand scopes in a follow-up and document the new consent screen.

### Authorization URL Parameters

```text
response_type=code
client_id=app_EMoamEEZ73f0CkXaXp7hrann
redirect_uri=http://localhost:1455/auth/callback
scope=openid profile email offline_access
code_challenge=<S256 PKCE challenge>
code_challenge_method=S256
state=<random CSRF state>
id_token_add_organizations=true
codex_cli_simplified_flow=true
originator=zed
```

Optional future parameter if a workspace restriction is added:

```text
allowed_workspace_id=<chatgpt_account_id>
```

### Token Exchange Request

Form-encoded POST to `https://auth.openai.com/oauth/token`:

```text
grant_type=authorization_code
client_id=app_EMoamEEZ73f0CkXaXp7hrann
code=<authorization code>
code_verifier=<PKCE verifier>
redirect_uri=http://localhost:1455/auth/callback
```

### Refresh Request

Form-encoded POST to `https://auth.openai.com/oauth/token`:

```text
grant_type=refresh_token
client_id=app_EMoamEEZ73f0CkXaXp7hrann
refresh_token=<refresh token>
```

### Stored Credential Model

Store a JSON blob through `CredentialsProvider` under a Codex-specific key, not under the existing OpenAI API URL.

Suggested key:

```text
https://chatgpt.com/backend-api/codex/oauth
```

All credential-bearing structs **must** have a manual `Debug` impl that redacts secrets. Pattern from `crates/context_server/src/oauth.rs:159-175`:

```rust
#[derive(Clone, Serialize, Deserialize)]
struct OpenAiCodexCredentials {
    access_token: String,
    refresh_token: String,
    expires_at_ms: u64,
    /// May be `None` if JWT claim extraction fails. The first request will
    /// surface a sign-in-required error in that case.
    account_id: Option<String>,
}

impl std::fmt::Debug for OpenAiCodexCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCodexCredentials")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("account_id", &self.account_id)
            .finish()
    }
}
```

```rust
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    id_token: Option<String>,
}

impl std::fmt::Debug for TokenResponse { /* redact access_token, refresh_token, id_token */ }
```

Account ID extraction order:

- `id_token` claim `chatgpt_account_id`
- `id_token` claim object `https://api.openai.com/auth.chatgpt_account_id`
- `id_token` nested claim object `https://api.openai.com/auth: { chatgpt_account_id }`
- `id_token` claim `organizations[0].id`
- same claim locations in `access_token` as fallback

If all extractions fail, **do not fail sign-in.** Store credentials with `account_id = None` and surface a clear post-auth error on the first Codex request explaining that the ChatGPT account ID could not be determined.

### Codex Request Headers

For `POST https://chatgpt.com/backend-api/codex/responses`:

```text
Authorization: Bearer <access_token>
ChatGPT-Account-ID: <account_id>
Accept: text/event-stream
Content-Type: application/json
originator: zed
```

Consider including only if validation indicates it is required:

```text
OpenAI-Beta: responses=experimental
```

### Codex Request Body

Codex backend expects Responses-style payloads with Codex-specific fields:

```json
{
  "model": "gpt-5.3-codex",
  "instructions": "<non-empty system/developer instructions>",
  "input": [],
  "tools": [],
  "tool_choice": "auto",
  "parallel_tool_calls": true,
  "reasoning": null,
  "store": false,
  "stream": true,
  "prompt_cache_key": "<optional thread id>"
}
```

Important details:

- `instructions` must be non-empty.
- `stream` must be `true`.
- `store` should be `false`.
- Existing system messages should become `instructions` rather than regular input messages.
- Initially omit `max_output_tokens` for Codex (matches `opencode` and Codex CLI).
- `prompt_cache_key` carries Zed's internal thread UUID. Worth a privacy review note — confirm with product before merging.
- Preserve tool conversion and response stream mapping where compatible.

### Refresh Stampede Prevention

Concurrent agent requests must not each trigger a refresh. Use the same `future::Shared<Task<...>>` pattern as `ApiKeyState::load_task` (`crates/language_model/src/api_key.rs:23`):

```rust
struct State {
    credentials: Option<OpenAiCodexCredentials>,
    refresh_task: Option<future::Shared<Task<Result<(), AuthenticateError>>>>,
    /* ... */
}

impl State {
    fn ensure_fresh_token(&mut self, cx: &mut Context<Self>) -> Task<Result<(), AuthenticateError>> {
        if !self.token_needs_refresh() {
            return Task::ready(Ok(()));
        }
        let task = if let Some(task) = &self.refresh_task {
            task.clone()
        } else {
            let task = Self::spawn_refresh(/* ... */).shared();
            self.refresh_task = Some(task.clone());
            task
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, _| this.refresh_task = None).ok();
            result
        })
    }
}
```

Use a 60-second safety window: refresh if `expires_at - now < 60s`. (MCP's `McpOAuthTokenProvider` uses 30s; the larger window here covers a full agent turn's worth of dispatch before reaching the network.)

### Logging / Internal Observability

Use existing Zed logging only. Examples:

```rust
log::info!("OpenAI Codex OAuth sign-in started");
log::info!("OpenAI Codex OAuth sign-in completed");
log::warn!("OpenAI Codex token refresh failed: {err}");
log::error!("OpenAI Codex request failed with status {status}");
```

Do not log:

- access tokens
- refresh tokens
- auth codes
- full callback URLs
- raw JWTs
- credential JSON blobs

Enforced structurally: every credential-bearing type has a manual `Debug` impl that redacts secrets.

## Implementation Order

### Milestone 0: Lift Shared OAuth Helpers

Promote the truly-generic OAuth helpers out of `crates/context_server/src/oauth.rs` so the Codex provider can reuse them.

- [ ] Identify generic helpers: `generate_pkce_challenge`, `OAuthCallback::parse_query`, `start_callback_server`, `validate_oauth_url`, `require_https_or_loopback`, the redacted-`Debug` token types (`OAuthTokens`, `TokenResponse`, `PkceChallenge`).
- [ ] Decide on relocation strategy:
  - **Option A (preferred):** new `crates/oauth_utils` crate with these symbols. Both `context_server` and `language_models` depend on it.
  - **Option B:** keep them in `context_server::oauth` but mark them `pub` and add a doc note. Lighter change, more coupling.
- [ ] Update `context_server::oauth` to consume the relocated helpers.
- [ ] Verify all existing `context_server` tests pass unchanged (`./script/clippy -p context_server` + targeted tests).

The Codex provider must work with a **fixed-port** callback server, while MCP uses an **ephemeral-port** server. Either:

- [ ] Add a port parameter to `start_callback_server(port: u16)` (0 means ephemeral), or
- [ ] Keep `start_callback_server` ephemeral-only and add `start_callback_server_on_port(port: u16)` for fixed-port use.

Validation:

- [ ] Existing context_server OAuth tests still pass.
- [ ] New crate (or extended API) compiles with no warnings.

### Milestone 1+2 (merged): Provider Skeleton + OAuth Credential State

Compilable on first commit. Skeleton includes credential state stubs so the `LanguageModelProvider` trait fully resolves.

- [ ] Create `crates/language_models/src/provider/open_ai_codex.rs`.
- [ ] Add `pub mod open_ai_codex;` to `crates/language_models/src/provider.rs`.
- [ ] Register `OpenAiCodexLanguageModelProvider` in `crates/language_models/src/language_models.rs`.
- [ ] Implement provider identity:
  - [ ] `id()` returns `openai_codex`.
  - [ ] `name()` returns `OpenAI Codex`.
  - [ ] `icon()` initially reuses `IconName::AiOpenAi` unless a distinct icon exists.
- [ ] Implement `provided_models()` from the Codex-local allow-list (see "Model Allow-List" above).
- [ ] Implement `default_model()` and `default_fast_model()` with conservative defaults.
- [ ] Add `OpenAiCodexSettings` to `AllLanguageModelSettings`.
- [ ] Define OAuth credential structs with manual redacted `Debug` impls:
  - `OpenAiCodexCredentials`
  - `TokenResponse`
- [ ] Implement provider-local `State`:
  - `credentials: Option<OpenAiCodexCredentials>`
  - `load_task: Option<future::Shared<Task<()>>>` (mirrors `ApiKeyState::load_task`)
  - `refresh_task: Option<future::Shared<Task<Result<(), AuthenticateError>>>>`
- [ ] Credential key constant: `https://chatgpt.com/backend-api/codex/oauth`.
- [ ] `load_credentials_if_needed` using `CredentialsProvider::read_credentials`.
- [ ] `store_credentials` using `CredentialsProvider::write_credentials`.
- [ ] `delete_credentials` using `CredentialsProvider::delete_credentials`.
- [ ] `is_authenticated()` returns true iff stored credentials are valid JSON and the refresh token is non-empty.
- [ ] `reset_credentials()` deletes Codex OAuth credentials.
- [ ] Stub all remaining `LanguageModelProvider` / `LanguageModel` trait methods so the crate compiles:
  - `authenticate` returns a task that loads credentials.
  - `configuration_view` returns a placeholder view (real UI in Milestone 7).
  - `LanguageModel::stream_completion` returns `Err(AuthenticationError { provider, message: "Sign in with ChatGPT to use OpenAI Codex" })` when not authenticated and a `todo!()` (or stub error) when authenticated.

Validation:

- [ ] `cargo check -p language_models` passes.
- [ ] Unit tests cover credential load/store/delete with a fake credentials provider.
- [ ] Invalid UTF-8 / invalid JSON stored-credential errors are surfaced as `LoadStatus::Error` and logged safely (no secret bytes in log output).
- [ ] Model selector lists "OpenAI Codex" with the five models when not authenticated.

### Milestone 3: PKCE and Browser OAuth Flow

Browser-based authorization-code flow with fixed-port callback server.

- [ ] Add required dependencies to `crates/language_models/Cargo.toml` if not pulled in transitively (`rand`, `sha2`, `tiny_http`, `url`, `urlencoding`, `serde_urlencoded`, `base64`).
- [ ] Reuse `generate_pkce_challenge()` from Milestone 0's shared helpers.
- [ ] Reuse `OAuthCallback::parse_query()` from Milestone 0's shared helpers.
- [ ] Implement random OAuth state generation (`base64::URL_SAFE_NO_PAD` of 32 random bytes).
- [ ] Implement **fixed-port** callback server at `127.0.0.1:1455` with `bind_server()` semantics matching Codex CLI (`/home/shuv/repos/codex-cli/codex-rs/login/src/server.rs:529`):
  - `MAX_ATTEMPTS = 10`, `RETRY_DELAY = 200ms`.
  - On `AddrInUse`, attempt one HTTP `GET http://127.0.0.1:1455/cancel` to dislodge a previous Zed login server.
  - Sleep `RETRY_DELAY`, retry up to `MAX_ATTEMPTS` times.
  - On final failure: surface user-facing error "Sign in already in progress, or port 1455 is in use by another app. Quit other ChatGPT/Codex login flows and try again."
- [ ] Implement `/cancel` handler in our own callback server so a second sign-in attempt can take over.
- [ ] Build authorization URL with the parameters in "Authorization URL Parameters".
- [ ] Open authorization URL with `cx.open_url`. If `open_url` returns an error or there's reason to suspect it failed (e.g. headless session detection), continue silently.
- [ ] **Browser-fail fallback (UX):** the configuration view (Milestone 7) always shows the auth URL with a "Copy URL" button while sign-in is in progress, so users on remote/SSH sessions can copy the URL into a browser elsewhere. Hint text: "Tip: for SSH sessions, run `ssh -L 1455:localhost:1455 host` first."
- [ ] Parse callback path `/auth/callback`.
- [ ] Validate returned `state` against the generated state.
- [ ] Return minimal success/failure HTML to the browser. Reuse the styling from `context_server::oauth`'s response if available.
- [ ] Add timeout (use the shared `CALLBACK_TIMEOUT` of 2 minutes; consider raising to 5 minutes for first-time users).
- [ ] Add cancellation: if the configuration view is closed, drop the receiver and the server thread shuts down cleanly.
- [ ] Exchange authorization code for tokens (form-encoded POST to `oauth/token`).
- [ ] Extract account ID from JWT claims using the priority order in "Stored Credential Model". On failure, store with `account_id = None` and continue.
- [ ] Store credentials on success.

Validation:

- [ ] Unit tests cover authorize URL params (no secrets in the URL params themselves; just structure).
- [ ] Unit tests cover callback parser success/failure (reuse `OAuthCallback::parse_query` tests transitively).
- [ ] Unit tests cover state mismatch failure.
- [ ] Unit tests cover JWT account ID extraction variants — explicitly test the "all extractions fail" path returning `Ok(None)` rather than `Err`.
- [ ] Unit tests cover `bind_server` retry path (mock `tiny_http::Server::http` via dependency injection or just trust the manual test).
- [ ] Manual login flow opens browser and returns to Zed.
- [ ] Manual login flow with `--ssh` style session: copying the auth URL into a remote browser still completes the flow (assuming port forwarding).

### Milestone 4: Token Refresh

- [ ] Implement refresh request to `https://auth.openai.com/oauth/token`.
- [ ] Refresh if `expires_at_ms - now_ms < 60_000`.
- [ ] Update stored credentials after successful refresh.
- [ ] Preserve or re-extract `account_id` after refresh. If extraction now succeeds where it failed before, update the stored value.
- [ ] **Refresh-stampede prevention:** use `future::Shared<Task<Result<(), AuthenticateError>>>` stored on `State::refresh_task`. See "Refresh Stampede Prevention" above. Pattern mirrors `ApiKeyState::load_task` at `crates/language_model/src/api_key.rs:23`.
- [ ] If refresh fails due to invalid credentials (HTTP 400/401 with `invalid_grant`), clear stored credentials and surface a `AuthenticationError { provider, message: "Sign in expired. Please sign in again." }`.
- [ ] If refresh fails due to transport error, retry once before surfacing.

Validation:

- [ ] Unit test: expired credentials trigger refresh.
- [ ] Unit test: non-expired credentials do not refresh.
- [ ] Unit test: refresh failure returns a useful error and does not log secrets.
- [ ] Unit test: concurrent `ensure_fresh_token` calls only perform one token-endpoint request. (Use a fake HTTP client that counts calls.)
- [ ] Unit test: `invalid_grant` clears credentials and triggers re-sign-in.

### Milestone 5: Codex Request Conversion

Build a provider-local `CodexResponsesRequest` that serializes the Codex-specific body, reusing `open_ai::responses` types for items, tools, reasoning.

- [ ] Define `CodexResponsesRequest` (provider-local) with fields: `model`, `instructions`, `input`, `tools`, `tool_choice`, `parallel_tool_calls`, `reasoning`, `store`, `stream`, `prompt_cache_key`.
- [ ] Implement conversion `into_codex_responses_request(request: LanguageModelRequest, model_id: &str, ...) -> CodexResponsesRequest`. Sibling to `into_open_ai_response` in `crates/open_ai/src/completion.rs` but in the provider crate.
- [ ] Convert system messages into a single non-empty `instructions` string.
- [ ] Remove converted system messages from `input`.
- [ ] Use a safe default instruction if no system message exists (e.g. `"You are an AI coding assistant."`).
- [ ] Preserve user/assistant/tool input ordering.
- [ ] Preserve tool definitions.
- [ ] Preserve tool choice where supported.
- [ ] Set `parallel_tool_calls = true` for requests with tools, `false`/omitted otherwise.
- [ ] Set `stream = true`.
- [ ] Set `store = false`.
- [ ] Omit `max_output_tokens` initially.
- [ ] Preserve `prompt_cache_key` from thread ID.

Validation:

- [ ] Snapshot/unit test generated JSON for a simple user request.
- [ ] Snapshot/unit test generated JSON with system instructions.
- [ ] Snapshot/unit test generated JSON with tool use and tool results.
- [ ] Assert `instructions` is non-empty.
- [ ] Assert `store == false` and `stream == true`.
- [ ] Assert `max_output_tokens` is absent.

### Milestone 5b: Refactor `stream_response` to Extract SSE Parser

Avoid duplicating SSE handling between OpenAI and Codex.

- [ ] In `crates/open_ai/src/responses.rs`, extract the SSE-parsing core of `stream_response` into a public helper:

  ```rust
  pub fn parse_responses_sse_stream(
      body: AsyncBody,
  ) -> BoxStream<'static, Result<StreamEvent>>;
  ```

  (or a function that takes a successful `Response<AsyncBody>` and returns the same stream)
- [ ] Extract the non-streaming-response synthesis path into a public helper too if needed.
- [ ] `stream_response` becomes a thin wrapper: build request → send → on success, return `parse_responses_sse_stream(response.into_body())`; on error, current error path.
- [ ] Verify existing OpenAI provider behavior is unchanged via existing tests in `crates/open_ai`.

Validation:

- [ ] `./script/clippy -p open_ai` passes.
- [ ] All `open_ai` unit tests pass with no behavior changes.

### Milestone 6: Codex HTTP Streaming

- [ ] Implement provider-local `stream_codex_response` that:
  - Calls `ensure_fresh_token()` first.
  - Reads access token and account ID from state.
  - Returns `AuthenticationError { provider, message: "Sign in with ChatGPT to use OpenAI Codex" }` if not authenticated.
  - Returns `AuthenticationError { provider, message: "ChatGPT account ID could not be determined; please sign out and sign in again." }` if `account_id` is `None`.
  - Builds request: POST `https://chatgpt.com/backend-api/codex/responses` with all required headers.
  - On success, calls `parse_responses_sse_stream` from Milestone 5b.
- [ ] Map parsed `open_ai::responses::StreamEvent` through the existing `OpenAiResponseEventMapper` from `crates/open_ai/src/completion.rs`.
- [ ] On 401 response, attempt one forced refresh + retry (mirrors `OAuthTokenProvider::try_refresh` semantics).
- [ ] On 403 response with Cloudflare-style HTML body, surface a distinct error pointing to Milestone 8 follow-up: "ChatGPT backend rejected the request (Cloudflare). This may need cookie support — please report."

Validation:

- [ ] Fake HTTP client test asserts URL, headers (`Authorization`, `ChatGPT-Account-ID`, `Accept: text/event-stream`, `originator`), and body shape.
- [ ] Fake SSE test verifies stream events map to `LanguageModelCompletionEvent`.
- [ ] Error response test includes status and a safe body excerpt without secrets.
- [ ] 401 retry test: first request returns 401, refresh fires, second request succeeds.
- [ ] Sign-out-mid-request test: receiving stream is cancelled cleanly.

### Milestone 7: Provider Configuration UI

- [ ] Add `ConfigurationView` for `OpenAiCodexLanguageModelProvider`.
- [ ] States to render:
  - [ ] loading credentials
  - [ ] not signed in
  - [ ] sign-in in progress (shows auth URL with Copy URL button + SSH hint)
  - [ ] signed in
  - [ ] error
- [ ] Not-signed-in UI explains:
  - [ ] this uses ChatGPT Plus/Pro Codex access
  - [ ] this is distinct from OpenAI Platform API keys
  - [ ] link to the existing OpenAI API-key provider for users without a ChatGPT subscription
- [ ] Add button: `Sign in with ChatGPT`.
- [ ] Sign-in-in-progress UI:
  - Shows the authorization URL with a "Copy URL" button.
  - Shows hint: "If your browser didn't open, copy the URL above. For SSH sessions, run `ssh -L 1455:localhost:1455 host` first."
  - Shows a "Cancel" button that drops the receiver and shuts down the callback server.
- [ ] Signed-in UI uses `ConfiguredApiCard::new("ChatGPT account connected")` (or similar, with the account ID display if available).
- [ ] Signed-in UI provides `Sign Out` behavior.
- [ ] **Cross-link the existing OpenAI API-key UI** in `crates/language_models/src/provider/open_ai.rs`: add a small note pointing ChatGPT Plus/Pro users to the OpenAI Codex provider.

Validation:

- [ ] Visual/manual test each UI state.
- [ ] Existing OpenAI API-key UI still renders as before except for the intentional cross-link copy.
- [ ] Cancelling sign-in mid-flow returns to the not-signed-in state without error.

### Milestone 8: Optional Cloudflare Cookie Handling Assessment

Codex CLI has explicit Cloudflare cookie handling in `/home/shuv/repos/codex-cli/codex-rs/codex-client/src/chatgpt_cloudflare_cookies.rs`.

First implementation should try without this because Zed's existing `HttpClient` abstraction does not expose cookie-jar hooks and several lightweight implementations work without custom handling.

- [ ] Manually validate authenticated Codex request against the real backend.
- [ ] If a 403 with Cloudflare-style HTML body or `cf-mitigated` / `cf-ray` headers appears, investigate one of:
  - Add a dedicated request path using a `reqwest` client with a Cloudflare-only cookie jar.
  - Extend Zed's HTTP client construction carefully, avoiding storage of ChatGPT session/auth cookies.

Validation:

- [ ] Confirm a real Codex request succeeds without custom cookie jar, **or** document required follow-up work and link to a tracking issue before merging.

### Milestone 9: Tests and Validation

- [ ] OAuth unit tests (PKCE, state, callback parser, JWT extraction).
- [ ] Credential state unit tests (load, store, delete, invalid JSON).
- [ ] Codex request JSON unit tests.
- [ ] Fake HTTP client tests for request URL/headers/body.
- [ ] Stream parsing/mapping test if practical.
- [ ] Configuration view smoke tests if matching project test patterns exist.
- [ ] Run targeted formatting/checks.
- [ ] Run targeted clippy:

  ```sh
  ./script/clippy -p language_models -p open_ai -p context_server
  ```

  (include `context_server` because Milestone 0 touched it; add the new `oauth_utils` crate if Option A was chosen)
- [ ] Manually validate:
  - [ ] sign in with ChatGPT
  - [ ] select OpenAI Codex provider/model
  - [ ] run a simple Zed Agent prompt
  - [ ] wait/force refresh path and validate a second request
  - [ ] sign out and confirm requests require sign-in again
  - [ ] Cloudflare check: confirm request succeeds and the response body is JSON SSE, not HTML. If HTML, treat as Milestone 8 blocker.
  - [ ] (If feasible) test on a remote SSH session with `ssh -L 1455:localhost:1455`.

## Potential Follow-Ups

- [ ] Add device-code/headless auth flow using Opencode's reference:
  - `POST https://auth.openai.com/api/accounts/deviceauth/usercode`
  - poll `POST https://auth.openai.com/api/accounts/deviceauth/token`
  - exchange returned authorization code with redirect URI `https://auth.openai.com/deviceauth/callback`
- [ ] Add "Import from Codex CLI" from `~/.codex/auth.json`.
- [ ] Add missing Codex model variants if product wants parity:
  - `gpt-5.1-codex`
  - `gpt-5.1-codex-mini`
  - `gpt-5.1-codex-max`
  - `gpt-5.4-mini`
- [ ] Add workspace/account selection if OpenAI returns multiple organizations.
- [ ] Add explicit FedRAMP routing if needed, matching Codex CLI's `X-OpenAI-Fedramp` header behavior.
- [ ] Coordinate with OpenAI to add `originator: "zed"` to Codex's first-party allow-list (`is_first_party_originator` in `default_client.rs`). Until then, expect non-first-party rate limits and telemetry treatment.
- [ ] Generalize `InjectHeaderClient` from `opencode.rs` and lift it to `crates/http_client/` if other providers need multi-header injection.
- [ ] Add an `available_models` block to `OpenAiCodexSettings` if users need to register custom Codex model IDs.

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Codex backend request shape differs from OpenAI Responses API | Requests fail with 400s | Use provider-local request struct and snapshot-test against captured/reference shape. |
| Fixed port `1455` is already in use | Login fails | Codex CLI-style cancel/retry on `AddrInUse` (Milestone 3). |
| Access token expires mid-request | Request fails | Refresh before dispatch using 60s safety window; retry once on 401. |
| Concurrent requests trigger multiple refreshes | Extra auth traffic / token churn | `future::Shared<Task>` dedup in `State::refresh_task` (Milestone 4). |
| Existing OpenAI provider behavior regresses | User-facing API-key breakage | Keep provider separate; only refactor `stream_response` to extract a parser, leaving its public shape unchanged. |
| Secrets accidentally logged | Security issue | Manual `Debug` impls that redact secrets on every credential-bearing struct. Don't rely on grep. |
| Cloudflare cookies needed | Requests may fail despite valid OAuth | Validate manually in Milestone 8/9; add cookie jar only if needed. |
| `originator: "zed"` is not on Codex's first-party allow-list | Possible rate-limit / telemetry / feature differences vs Codex CLI | Document explicitly. Empirically validate. Coordinate with OpenAI as a follow-up. |
| Headless / SSH sessions can't open a browser | Sign-in blocked | Configuration UI shows the auth URL with Copy URL + SSH port-forward hint. Device-code flow as a follow-up. |
| Account ID extraction fails for some accounts | First request 401s | Treat as recoverable: store with `account_id = None`, surface a clear post-auth error. |
| `prompt_cache_key` sends Zed thread UUIDs to OpenAI | Privacy / telemetry leakage perception | Document in PR description; confirm with product before merging. Consider hashing or making opt-in if flagged. |
| Provider ID `openai_codex` baked into user settings | Renaming requires settings migration | Lock the ID before release; treat as public API. |
| Drift between SSE parser used by OpenAI vs Codex | Future stream-event additions miss one path | Milestone 5b extracts a single shared parser. |

## Review Checklist

- [ ] Existing OpenAI API-key provider still works.
- [ ] New provider is clearly named and distinct in UI.
- [ ] OAuth scopes match the minimal first-cut set, not Codex CLI's broader `api.connectors.*` set.
- [ ] Authorization URL params match the spec table.
- [ ] Stored credentials do not collide with API keys.
- [ ] Refresh token is persisted securely.
- [ ] Refresh dedup uses `future::Shared<Task>` (no stampede).
- [ ] `ChatGPT-Account-ID` header is present on Codex backend requests.
- [ ] Codex request body includes non-empty `instructions`, `store: false`, and `stream: true`, and omits `max_output_tokens`.
- [ ] All credential-bearing structs have manual redacted `Debug` impls. No tokens, codes, JWTs, or full callback URLs are logged.
- [ ] `LanguageModelCompletionError::AuthenticationError` (not `NoApiKey`) is used for the not-signed-in case.
- [ ] Errors propagate to UI with useful messages.
- [ ] Configuration UI shows the auth URL with Copy URL during sign-in for headless/SSH support.
- [ ] Configuration UI cross-links between OpenAI Codex (OAuth) and OpenAI (API-key) providers.
- [ ] `stream_response` was refactored to share an SSE parser with the Codex path.
- [ ] Milestone 0 helpers are used; no PKCE / callback-server / token-debug code is duplicated from `context_server::oauth`.
- [ ] Targeted tests and `./script/clippy -p language_models -p open_ai -p context_server` pass.

## Suggested PR Shape

One focused PR if kept scoped:

1. Milestone 0: lift shared OAuth helpers (own commit; small, low-risk).
2. Milestone 5b: extract `parse_responses_sse_stream` from `stream_response` (own commit; pure refactor, no behavior change).
3. Milestone 1+2: provider skeleton + OAuth credential state.
4. Milestone 3: PKCE and browser OAuth flow.
5. Milestone 4: token refresh with `future::Shared` dedup.
6. Milestone 5: Codex request conversion.
7. Milestone 6: Codex HTTP streaming.
8. Milestone 7: configuration UI + cross-links.
9. Tests.

If the PR grows past comfortable review size, split after step 2 (refactor PR) from steps 3–9 (feature PR). Avoid bundling optional import / device-code / model-expansion work into the first PR unless requested.
