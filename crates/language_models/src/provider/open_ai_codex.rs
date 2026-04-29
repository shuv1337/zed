use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use credentials_provider::CredentialsProvider;
use futures::{FutureExt, StreamExt, future};
use gpui::{
    AnyView, App, AsyncApp, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    Styled, Task, Window, div,
};
use http_client::{AsyncBody, HttpClient, Method, Request as HttpRequest};
use language_model::{
    AuthenticateError, ConfigurationViewTargetAgent, IconOrSvg, LanguageModel,
    LanguageModelCompletionError, LanguageModelCompletionEvent, LanguageModelId, LanguageModelName,
    LanguageModelProvider, LanguageModelProviderId, LanguageModelProviderName,
    LanguageModelProviderState, LanguageModelRequest, LanguageModelToolChoice, RateLimiter,
};
use open_ai::completion::{OpenAiResponseEventMapper, into_open_ai_response};
use open_ai::responses::{
    Request as ResponseRequest, StreamEvent as ResponsesStreamEvent, parse_responses_sse_stream,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use settings::Settings;
use sha2::{Digest, Sha256};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ui::{Button, ButtonStyle, ConfiguredApiCard, prelude::*};
use util::ResultExt;

const PROVIDER_ID: LanguageModelProviderId = LanguageModelProviderId::new("openai_codex");
const PROVIDER_NAME: LanguageModelProviderName = LanguageModelProviderName::new("OpenAI Codex");
const DEFAULT_API_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_AUTH_URL: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CREDENTIAL_KEY: &str = "https://chatgpt.com/backend-api/codex/oauth";
const ORIGINATOR: &str = "zed";
#[allow(dead_code)]
const REFRESH_WINDOW_MS: u64 = 60_000;

#[derive(Default, Clone, Debug, PartialEq)]
pub struct OpenAiCodexSettings {
    pub api_url: String,
    pub auth_url: String,
}

#[derive(Clone)]
struct CodexModel {
    id: &'static str,
    display_name: &'static str,
    max_tokens: u64,
    max_output: u64,
    supports_thinking: bool,
}

static CODEX_MODELS: LazyLock<Vec<CodexModel>> = LazyLock::new(|| {
    vec![
        CodexModel {
            id: "gpt-5-codex",
            display_name: "GPT-5 Codex",
            max_tokens: 272_000,
            max_output: 128_000,
            supports_thinking: false,
        },
        CodexModel {
            id: "gpt-5.2",
            display_name: "GPT-5.2",
            max_tokens: 400_000,
            max_output: 128_000,
            supports_thinking: false,
        },
        CodexModel {
            id: "gpt-5.2-codex",
            display_name: "GPT-5.2 Codex",
            max_tokens: 400_000,
            max_output: 128_000,
            supports_thinking: false,
        },
        CodexModel {
            id: "gpt-5.3-codex",
            display_name: "GPT-5.3 Codex",
            max_tokens: 400_000,
            max_output: 128_000,
            supports_thinking: true,
        },
        CodexModel {
            id: "gpt-5.4",
            display_name: "GPT-5.4",
            max_tokens: 1_050_000,
            max_output: 128_000,
            supports_thinking: false,
        },
    ]
});

#[derive(Clone, Serialize, Deserialize)]
struct OpenAiCodexCredentials {
    access_token: String,
    refresh_token: String,
    expires_at_ms: u64,
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

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    id_token: Option<String>,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &"[redacted]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("expires_in", &self.expires_in)
            .field("id_token", &self.id_token.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

pub struct OpenAiCodexLanguageModelProvider {
    http_client: Arc<dyn HttpClient>,
    state: Entity<State>,
}

pub struct State {
    credentials: Option<OpenAiCodexCredentials>,
    credentials_loaded: bool,
    auth_url: Option<String>,
    error: Option<String>,
    credentials_provider: Arc<dyn CredentialsProvider>,
    http_client: Arc<dyn HttpClient>,
}

impl State {
    fn is_authenticated(&self) -> bool {
        self.credentials
            .as_ref()
            .is_some_and(|credentials| !credentials.refresh_token.is_empty())
    }

    fn load_credentials_if_needed(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), AuthenticateError>> {
        if self.credentials_loaded {
            return Task::ready(Ok(()));
        }
        let credentials_provider = self.credentials_provider.clone();
        cx.spawn(async move |this, cx| {
            let result = async {
                let credentials = credentials_provider
                    .read_credentials(CREDENTIAL_KEY, cx)
                    .await
                    .map_err(|error| AuthenticateError::Other(error))?;
                if let Some((_, bytes)) = credentials {
                    let credentials: OpenAiCodexCredentials = serde_json::from_slice(&bytes)
                        .map_err(|error| {
                            AuthenticateError::Other(anyhow!(
                                "invalid OpenAI Codex credentials: {error}"
                            ))
                        })?;
                    this.update(cx, |this, cx| {
                        this.credentials = Some(credentials);
                        this.credentials_loaded = true;
                        this.error = None;
                        cx.notify();
                    })
                    .map_err(|error| AuthenticateError::Other(error))?;
                } else {
                    this.update(cx, |this, cx| {
                        this.credentials = None;
                        this.credentials_loaded = true;
                        cx.notify();
                    })
                    .map_err(|error| AuthenticateError::Other(error))?;
                }
                Ok(())
            }
            .await;
            result
        })
    }

    #[allow(dead_code)]
    fn store_credentials(
        &mut self,
        credentials: OpenAiCodexCredentials,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.credentials = Some(credentials.clone());
        self.credentials_loaded = true;
        cx.notify();
        let credentials_provider = self.credentials_provider.clone();
        cx.spawn(async move |_, cx| {
            let bytes = serde_json::to_vec(&credentials)?;
            credentials_provider
                .write_credentials(CREDENTIAL_KEY, "oauth", &bytes, cx)
                .await
        })
    }

    fn delete_credentials(&mut self, cx: &mut Context<Self>) -> Task<Result<()>> {
        self.credentials = None;
        self.credentials_loaded = true;
        self.error = None;
        cx.notify();
        let credentials_provider = self.credentials_provider.clone();
        cx.spawn(async move |_, cx| {
            credentials_provider
                .delete_credentials(CREDENTIAL_KEY, cx)
                .await
        })
    }

    #[allow(dead_code)]
    fn ensure_fresh_token(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), AuthenticateError>> {
        let Some(credentials) = self.credentials.clone() else {
            return Task::ready(Err(AuthenticateError::Other(anyhow!(
                "Sign in with ChatGPT to use OpenAI Codex"
            ))));
        };
        if credentials.expires_at_ms > now_ms().saturating_add(REFRESH_WINDOW_MS) {
            return Task::ready(Ok(()));
        }
        let http_client = self.http_client.clone();
        let credentials_provider = self.credentials_provider.clone();
        let auth_url = OpenAiCodexLanguageModelProvider::auth_url(cx);
        cx.spawn(async move |this, cx| {
            let result = refresh_credentials(http_client.as_ref(), &auth_url, &credentials).await;
            match result {
                Ok(credentials) => {
                    let bytes = serde_json::to_vec(&credentials)
                        .map_err(|error| AuthenticateError::Other(error.into()))?;
                    credentials_provider
                        .write_credentials(CREDENTIAL_KEY, "oauth", &bytes, cx)
                        .await
                        .map_err(|error| AuthenticateError::Other(error))?;
                    this.update(cx, |this, cx| {
                        this.credentials = Some(credentials);
                        this.error = None;
                        cx.notify();
                    })
                    .map_err(|error| AuthenticateError::Other(error))?;
                    Ok(())
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.error = Some(error.to_string());
                        cx.notify();
                    })
                    .ok();
                    Err(AuthenticateError::Other(error))
                }
            }
        })
    }

    fn sign_in(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), AuthenticateError>> {
        log::info!("OpenAI Codex OAuth sign-in started");
        let pkce = generate_pkce_challenge();
        let state = random_url_safe(32);
        let auth_url = build_authorization_url(
            &OpenAiCodexLanguageModelProvider::auth_url(cx),
            &pkce.challenge,
            &state,
        );
        self.auth_url = Some(auth_url.clone());
        self.error = None;
        cx.notify();
        cx.open_url(&auth_url);
        let http_client = self.http_client.clone();
        let token_url = format!(
            "{}/oauth/token",
            OpenAiCodexLanguageModelProvider::auth_url(cx).trim_end_matches('/')
        );
        let credentials_provider = self.credentials_provider.clone();
        cx.spawn_in(window, async move |this, cx| {
            let callback = cx
                .background_spawn(async move { wait_for_callback() })
                .await
                .map_err(|error| AuthenticateError::Other(error))?;
            if callback.state != state {
                return Err(AuthenticateError::Other(anyhow!(
                    "OAuth state did not match"
                )));
            }
            let credentials = exchange_code_for_credentials(
                http_client.as_ref(),
                &token_url,
                &callback.code,
                &pkce.verifier,
            )
            .await
            .map_err(|error| AuthenticateError::Other(error))?;
            let bytes = serde_json::to_vec(&credentials)
                .map_err(|error| AuthenticateError::Other(error.into()))?;
            credentials_provider
                .write_credentials(CREDENTIAL_KEY, "oauth", &bytes, cx)
                .await
                .map_err(|error| AuthenticateError::Other(error))?;
            this.update(cx, |this, cx| {
                this.credentials = Some(credentials);
                this.credentials_loaded = true;
                this.auth_url = None;
                this.error = None;
                cx.notify();
            })
            .map_err(|error| AuthenticateError::Other(error))?;
            log::info!("OpenAI Codex OAuth sign-in completed");
            Ok(())
        })
    }
}

impl OpenAiCodexLanguageModelProvider {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        let state = cx.new(|_| State {
            credentials: None,
            credentials_loaded: false,
            auth_url: None,
            error: None,
            credentials_provider,
            http_client: http_client.clone(),
        });
        Self { http_client, state }
    }

    fn settings(cx: &App) -> &crate::AllLanguageModelSettings {
        crate::AllLanguageModelSettings::get_global(cx)
    }
    fn api_url(cx: &App) -> SharedString {
        let api_url = &Self::settings(cx).openai_codex.api_url;
        if api_url.is_empty() {
            DEFAULT_API_URL.into()
        } else {
            api_url.as_str().into()
        }
    }
    fn auth_url(cx: &App) -> SharedString {
        let auth_url = &Self::settings(cx).openai_codex.auth_url;
        if auth_url.is_empty() {
            DEFAULT_AUTH_URL.into()
        } else {
            auth_url.as_str().into()
        }
    }
    fn create_language_model(&self, model: CodexModel) -> Arc<dyn LanguageModel> {
        Arc::new(OpenAiCodexLanguageModel {
            id: LanguageModelId::from(model.id.to_string()),
            model,
            state: self.state.clone(),
            http_client: self.http_client.clone(),
            request_limiter: RateLimiter::new(4),
        })
    }
}

impl LanguageModelProviderState for OpenAiCodexLanguageModelProvider {
    type ObservableEntity = State;
    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for OpenAiCodexLanguageModelProvider {
    fn id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }
    fn name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }
    fn icon(&self) -> IconOrSvg {
        IconOrSvg::Icon(IconName::AiOpenAi)
    }
    fn default_model(&self, _cx: &App) -> Option<Arc<dyn LanguageModel>> {
        Some(self.create_language_model(CODEX_MODELS[3].clone()))
    }
    fn default_fast_model(&self, _cx: &App) -> Option<Arc<dyn LanguageModel>> {
        Some(self.create_language_model(CODEX_MODELS[0].clone()))
    }
    fn provided_models(&self, _cx: &App) -> Vec<Arc<dyn LanguageModel>> {
        CODEX_MODELS
            .iter()
            .cloned()
            .map(|model| self.create_language_model(model))
            .collect()
    }
    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).is_authenticated()
    }
    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        self.state
            .update(cx, |state, cx| state.load_credentials_if_needed(cx))
    }
    fn configuration_view(
        &self,
        _target_agent: ConfigurationViewTargetAgent,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        cx.new(|cx| ConfigurationView::new(self.state.clone(), window, cx))
            .into()
    }
    fn reset_credentials(&self, cx: &mut App) -> Task<Result<()>> {
        self.state
            .update(cx, |state, cx| state.delete_credentials(cx))
    }
}

struct OpenAiCodexLanguageModel {
    id: LanguageModelId,
    model: CodexModel,
    state: Entity<State>,
    http_client: Arc<dyn HttpClient>,
    request_limiter: RateLimiter,
}

impl OpenAiCodexLanguageModel {
    fn stream_response(
        &self,
        request: ResponseRequest,
        cx: &AsyncApp,
    ) -> future::BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<'static, Result<ResponsesStreamEvent>>,
            LanguageModelCompletionError,
        >,
    > {
        let http_client = self.http_client.clone();
        let state = self.state.clone();
        let api_url = self
            .state
            .read_with(cx, |_, cx| OpenAiCodexLanguageModelProvider::api_url(cx));
        let credentials = state.read_with(cx, |state, _| state.credentials.clone());
        let future = self.request_limiter.stream(async move {
            let credentials = credentials
                .ok_or_else(|| LanguageModelCompletionError::AuthenticationError { provider: PROVIDER_NAME, message: "Sign in with ChatGPT to use OpenAI Codex".into() })?;
            let Some(account_id) = credentials.account_id.clone() else {
                return Err(LanguageModelCompletionError::AuthenticationError { provider: PROVIDER_NAME, message: "ChatGPT account ID could not be determined; please sign out and sign in again.".into() });
            };
            send_codex_request(http_client.as_ref(), &api_url, &credentials.access_token, &account_id, request).await
                .map_err(|error| LanguageModelCompletionError::Other(error))
        });
        async move { Ok(future.await?.boxed()) }.boxed()
    }
}

impl LanguageModel for OpenAiCodexLanguageModel {
    fn id(&self) -> LanguageModelId {
        self.id.clone()
    }
    fn name(&self) -> LanguageModelName {
        LanguageModelName::from(self.model.display_name.to_string())
    }
    fn provider_id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }
    fn provider_name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }
    fn supports_tools(&self) -> bool {
        true
    }
    fn supports_images(&self) -> bool {
        true
    }
    fn supports_tool_choice(&self, choice: LanguageModelToolChoice) -> bool {
        matches!(
            choice,
            LanguageModelToolChoice::Auto
                | LanguageModelToolChoice::Any
                | LanguageModelToolChoice::None
        )
    }
    fn supports_streaming_tools(&self) -> bool {
        true
    }
    fn supports_thinking(&self) -> bool {
        self.model.supports_thinking
    }
    fn supports_split_token_display(&self) -> bool {
        true
    }
    fn telemetry_id(&self) -> String {
        format!("openai_codex/{}", self.model.id)
    }
    fn max_token_count(&self) -> u64 {
        self.model.max_tokens
    }
    fn max_output_tokens(&self) -> Option<u64> {
        Some(self.model.max_output)
    }
    fn stream_completion(
        &self,
        request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> future::BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<
                'static,
                Result<LanguageModelCompletionEvent, LanguageModelCompletionError>,
            >,
            LanguageModelCompletionError,
        >,
    > {
        let request = into_codex_response(request, self.model.id, self.model.supports_thinking);
        let completions = self.stream_response(request, cx);
        async move {
            let mapper = OpenAiResponseEventMapper::new();
            Ok(mapper.map_stream(completions.await?).boxed())
        }
        .boxed()
    }
}

fn into_codex_response(
    request: LanguageModelRequest,
    model_id: &str,
    supports_thinking: bool,
) -> ResponseRequest {
    let mut request = into_open_ai_response(
        request,
        model_id,
        true,
        true,
        None,
        supports_thinking.then_some(open_ai::ReasoningEffort::Medium),
    );
    request.instructions = Some("You are an AI coding assistant.".to_string());
    request.stream = true;
    request.store = Some(false);
    request.max_output_tokens = None;
    request
}

async fn send_codex_request(
    client: &dyn HttpClient,
    api_url: &str,
    access_token: &str,
    account_id: &str,
    request: ResponseRequest,
) -> Result<futures::stream::BoxStream<'static, Result<ResponsesStreamEvent>>> {
    let uri = format!("{}/responses", api_url.trim_end_matches('/'));
    let request = HttpRequest::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Authorization", format!("Bearer {}", access_token.trim()))
        .header("ChatGPT-Account-ID", account_id)
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json")
        .header("originator", ORIGINATOR)
        .body(AsyncBody::from(serde_json::to_string(&request)?))?;
    let mut response = client.send(request).await?;
    if response.status().is_success() {
        Ok(parse_responses_sse_stream(response.into_body()))
    } else {
        use futures::AsyncReadExt;
        let status = response.status();
        let mut body = String::new();
        response.body_mut().read_to_string(&mut body).await?;
        if status.as_u16() == 403 && body.to_ascii_lowercase().contains("cloudflare") {
            anyhow::bail!(
                "ChatGPT backend rejected the request (Cloudflare). This may need cookie support — please report."
            );
        }
        anyhow::bail!(
            "OpenAI Codex request failed with status {status}: {}",
            safe_excerpt(&body)
        );
    }
}

#[derive(Debug)]
struct PkceChallenge {
    verifier: String,
    challenge: String,
}
fn generate_pkce_challenge() -> PkceChallenge {
    let verifier = random_url_safe(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    PkceChallenge {
        verifier,
        challenge,
    }
}
fn random_url_safe(bytes: usize) -> String {
    let mut random = vec![0; bytes];
    rand::rng().fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}
fn build_authorization_url(auth_url: &str, challenge: &str, state: &str) -> String {
    format!(
        "{}/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator={}",
        auth_url.trim_end_matches('/'),
        CLIENT_ID,
        urlencoding::encode(REDIRECT_URI),
        urlencoding::encode("openid profile email offline_access"),
        challenge,
        state,
        ORIGINATOR
    )
}

struct OAuthCallback {
    code: String,
    state: String,
}
fn wait_for_callback() -> Result<OAuthCallback> {
    let server = tiny_http::Server::http("127.0.0.1:1455").map_err(|error| anyhow!("Sign in already in progress, or port 1455 is in use by another app. Quit other ChatGPT/Codex login flows and try again. ({error})"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("Timed out waiting for OpenAI Codex sign-in");
        }
        if let Some(request) = server.recv_timeout(Duration::from_millis(200))? {
            let url = request.url().to_string();
            if url.starts_with("/cancel") {
                request
                    .respond(tiny_http::Response::from_string("Cancelled").with_status_code(200))
                    .ok();
                anyhow::bail!("OpenAI Codex sign-in cancelled");
            }
            if !url.starts_with("/auth/callback") {
                request
                    .respond(tiny_http::Response::from_string("Not found").with_status_code(404))
                    .ok();
                continue;
            }
            let query = url
                .split_once('?')
                .map(|(_, query)| query)
                .unwrap_or_default();
            let params: std::collections::HashMap<String, String> =
                serde_urlencoded::from_str(query)?;
            if let Some(error) = params.get("error") {
                anyhow::bail!("OpenAI OAuth error: {error}");
            }
            let code = params
                .get("code")
                .cloned()
                .ok_or_else(|| anyhow!("OAuth callback was missing code"))?;
            let state = params
                .get("state")
                .cloned()
                .ok_or_else(|| anyhow!("OAuth callback was missing state"))?;
            request.respond(tiny_http::Response::from_string("<html><body><h1>OpenAI Codex sign-in complete</h1><p>You can close this tab.</p></body></html>").with_status_code(200)).ok();
            return Ok(OAuthCallback { code, state });
        }
    }
}

async fn exchange_code_for_credentials(
    client: &dyn HttpClient,
    token_url: &str,
    code: &str,
    verifier: &str,
) -> Result<OpenAiCodexCredentials> {
    let body = serde_urlencoded::to_string([
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", REDIRECT_URI),
    ])?;
    let token_response = send_token_request(client, token_url, body).await?;
    credentials_from_token_response(token_response, None)
}

#[allow(dead_code)]
async fn refresh_credentials(
    client: &dyn HttpClient,
    auth_url: &str,
    credentials: &OpenAiCodexCredentials,
) -> Result<OpenAiCodexCredentials> {
    let token_url = format!("{}/oauth/token", auth_url.trim_end_matches('/'));
    let body = serde_urlencoded::to_string([
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", credentials.refresh_token.as_str()),
    ])?;
    let token_response = send_token_request(client, &token_url, body).await?;
    credentials_from_token_response(token_response, Some(credentials))
}

async fn send_token_request(
    client: &dyn HttpClient,
    token_url: &str,
    body: String,
) -> Result<TokenResponse> {
    use futures::AsyncReadExt;
    let request = HttpRequest::builder()
        .method(Method::POST)
        .uri(token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(AsyncBody::from(body))?;
    let mut response = client.send(request).await?;
    let mut response_body = String::new();
    response
        .body_mut()
        .read_to_string(&mut response_body)
        .await?;
    if !response.status().is_success() {
        anyhow::bail!(
            "OpenAI OAuth token endpoint returned {}: {}",
            response.status(),
            safe_excerpt(&response_body)
        );
    }
    Ok(serde_json::from_str(&response_body)?)
}

fn credentials_from_token_response(
    token_response: TokenResponse,
    previous: Option<&OpenAiCodexCredentials>,
) -> Result<OpenAiCodexCredentials> {
    let refresh_token = token_response
        .refresh_token
        .clone()
        .or_else(|| previous.map(|credentials| credentials.refresh_token.clone()))
        .ok_or_else(|| anyhow!("OpenAI OAuth response did not include a refresh token"))?;
    let expires_in = token_response.expires_in.unwrap_or(3600);
    let account_id = token_response
        .id_token
        .as_deref()
        .and_then(extract_account_id_from_jwt)
        .or_else(|| extract_account_id_from_jwt(&token_response.access_token))
        .or_else(|| previous.and_then(|credentials| credentials.account_id.clone()));
    Ok(OpenAiCodexCredentials {
        access_token: token_response.access_token,
        refresh_token,
        expires_at_ms: now_ms().saturating_add(expires_in.saturating_mul(1000)),
        account_id,
    })
}

fn extract_account_id_from_jwt(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("chatgpt_account_id")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("https://api.openai.com/auth.chatgpt_account_id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("organizations")
                .and_then(Value::as_array)
                .and_then(|organizations| organizations.first())
                .and_then(|organization| organization.get("id"))
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn safe_excerpt(body: &str) -> String {
    body.chars().take(512).collect()
}

struct ConfigurationView {
    state: Entity<State>,
    load_task: Option<Task<()>>,
    sign_in_task: Option<Task<()>>,
}
impl ConfigurationView {
    fn new(state: Entity<State>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let load_task = Some(cx.spawn_in(window, {
            let state = state.clone();
            async move |this, cx| {
                state
                    .update(cx, |state, cx| state.load_credentials_if_needed(cx))
                    .await
                    .log_err();
                this.update(cx, |this, cx| {
                    this.load_task = None;
                    cx.notify();
                })
                .log_err();
            }
        }));
        Self {
            state,
            load_task,
            sign_in_task: None,
        }
    }
}
impl Render for ConfigurationView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let is_loading = self.load_task.is_some();
        let is_signing_in = self.sign_in_task.is_some() || state.auth_url.is_some();
        let auth_url = state.auth_url.clone();
        let error = state.error.clone();
        let signed_in = state.is_authenticated();
        div().flex().flex_col().gap_3().child(Label::new("OpenAI Codex uses ChatGPT Plus/Pro OAuth and is separate from OpenAI Platform API keys."))
            .when(is_loading, |this| this.child(Label::new("Loading credentials…")))
            .when(signed_in, |this| this.child(ConfiguredApiCard::new("ChatGPT account connected")))
            .when(!signed_in && !is_signing_in, |this| this.child(Button::new("sign-in-openai-codex", "Sign in with ChatGPT").style(ButtonStyle::Filled).on_click(cx.listener(|this, _, window, cx| {
                let task = this.state.update(cx, |state, cx| state.sign_in(window, cx));
                this.sign_in_task = Some(cx.spawn_in(window, async move |this, cx| {
                    if let Err(error) = task.await { this.update(cx, |this, cx| { this.state.update(cx, |state, cx| { state.error = Some(error.to_string()); state.auth_url = None; cx.notify(); }); this.sign_in_task = None; cx.notify(); }).ok(); }
                    else { this.update(cx, |this, cx| { this.sign_in_task = None; cx.notify(); }).ok(); }
                }));
            }))))
            .when_some(auth_url, |this, url| this.child(div().flex().flex_col().gap_1().child(Label::new("If your browser didn't open, copy this URL. For SSH sessions, run `ssh -L 1455:localhost:1455 host` first." )).child(Label::new(url))))
            .when(signed_in, |this| this.child(Button::new("sign-out-openai-codex", "Sign Out").on_click(cx.listener(|this, _, window, cx| {
                let task = this.state.update(cx, |state, cx| state.delete_credentials(cx));
                cx.spawn_in(window, async move |_, _| { task.await.log_err(); }).detach();
            }))))
            .when_some(error, |this, error| this.child(Label::new(error).color(Color::Error)))
    }
}
