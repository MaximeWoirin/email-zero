use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::config::{LlmConfig, RetryConfig};
use crate::error::{Error, Result};
use crate::ports::{JsonChatRequest, LlmClient};
use crate::retry::{sleep_tokio, with_retry};

/// Corps de la requete POST {base_url}/chat/completions (API OpenAI-compatible).
#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a [crate::ports::ChatMessage],
    temperature: f32,
    top_p: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    error: Option<ApiErrorBody>,
}

#[derive(serde::Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(serde::Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiErrorBody {
    message: String,
}

/// Parse le `response_format` stringifie (BATCH_SCHEMA) en objet JSON pour l'API.
/// Le schema est deja un objet `{"type": "json_schema", ...}` : on le reutilise tel quel.
fn response_format_from_schema(schema: &str) -> Result<serde_json::Value> {
    serde_json::from_str(schema).map_err(Error::from)
}

/// Client LLM reel : endpoint OpenAI-compatible (`{base_url}/chat/completions`),
/// cle API lue dans la variable d'env `api_key_env` (jamais logguee), timeout et
/// retry/backoff configurables. Structured output via `response_format`.
pub struct OpenAiLlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    reasoning_effort: Option<String>,
    retry: RetryConfig,
}

impl OpenAiLlmClient {
    /// `api_key_lookup` injecte pour les tests (aucun env reel requis).
    pub fn new(cfg: &LlmConfig, api_key_lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let api_key = api_key_lookup(&cfg.api_key_env)
            .ok_or_else(|| Error::MissingSecretEnv(cfg.api_key_env.clone()))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
            .build()
            .map_err(|e| Error::Llm(format!("client HTTP: {e}")))?;
        Ok(Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key,
            reasoning_effort: cfg.reasoning_effort.clone(),
            retry: cfg.retry.clone(),
        })
    }

    pub fn new_from_env(cfg: &LlmConfig) -> Result<Self> {
        Self::new(cfg, &|name| std::env::var(name).ok())
    }

    async fn post<T: DeserializeOwned>(
        &self,
        req: &JsonChatRequest,
        schema: Option<&str>,
    ) -> Result<T> {
        let response_format = match schema {
            Some(s) => Some(response_format_from_schema(s)?),
            None => None,
        };
        let body = ChatCompletionRequest {
            model: &req.model,
            messages: &req.messages,
            temperature: req.temperature,
            top_p: req.top_p,
            max_tokens: req.max_tokens,
            reasoning_effort: self.reasoning_effort.as_deref(),
            response_format,
        };
        let url = format!("{}/chat/completions", self.base_url);
        let http = self.http.clone();
        let api_key = self.api_key.clone();
        let payload = serde_json::to_value(&body)?;
        let parsed: ChatCompletionResponse = with_retry(
            &self.retry,
            || {
                let http = http.clone();
                let url = url.clone();
                let api_key = api_key.clone();
                let payload = payload.clone();
                async move {
                    let resp = http
                        .post(&url)
                        .bearer_auth(&api_key)
                        .json(&payload)
                        .send()
                        .await
                        .map_err(|e| Error::Llm(format!("requete HTTP: {e}")))?;
                    let status = resp.status();
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| Error::Llm(format!("lecture reponse: {e}")))?;
                    if !status.is_success() {
                        return Err(Error::Llm(format!(
                            "HTTP {status} depuis {url}: {}",
                            text.chars().take(500).collect::<String>()
                        )));
                    }
                    serde_json::from_str(&text).map_err(|e| {
                        Error::Llm(format!(
                            "reponse non JSON ({e}): {}",
                            text.chars().take(200).collect::<String>()
                        ))
                    })
                }
            },
            sleep_tokio,
        )
        .await?;
        if let Some(e) = parsed.error {
            return Err(Error::Llm(format!("erreur API: {}", e.message)));
        }
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| Error::Llm("reponse sans choix ni contenu".into()))?;
        serde_json::from_str(&content).map_err(|e| {
            Error::Llm(format!(
                "contenu du choix non deserialisable ({e}): {}",
                content.chars().take(300).collect::<String>()
            ))
        })
    }
}

impl LlmClient for OpenAiLlmClient {
    async fn chat_json<T: DeserializeOwned>(&self, req: JsonChatRequest) -> Result<T> {
        self.post(&req, None).await
    }

    async fn chat_json_schema<T: DeserializeOwned>(
        &self,
        req: JsonChatRequest,
        schema: &str,
    ) -> Result<T> {
        self.post(&req, Some(schema)).await
    }
}
