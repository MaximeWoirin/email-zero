use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub name: String,
    pub special_use: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MailHeader {
    pub subject: String,
    pub from: String,
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ParsedMail {
    pub header: MailHeader,
    pub body_text: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JsonChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f32,
    pub top_p: f32,
    pub max_tokens: u32,
    pub response_format_json_schema: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Toutes les operations IMAP passent par ce trait.
/// Vrai client (async-imap) en prod, fake in-memory en test.
#[allow(async_fn_in_trait)]
pub trait MailClient {
    async fn list_folders(&mut self) -> Result<Vec<Folder>>;
    async fn search_unseen(&mut self, folder: &str) -> Result<Vec<u32>>;
    async fn fetch_headers(&mut self, folder: &str, uid: u32) -> Result<MailHeader>;
    async fn fetch_body(&mut self, folder: &str, uid: u32) -> Result<ParsedMail>;
    async fn copy(&mut self, folder: &str, uid: u32, target: &str) -> Result<()>;
    async fn r#move(&mut self, folder: &str, uid: u32, target: &str) -> Result<()>;
}

/// Toute requete LLM passe par ce trait.
/// Vrai client (async-openai) en prod, fake scripte en test.
#[allow(async_fn_in_trait)]
pub trait LlmClient {
    async fn chat_json<T: DeserializeOwned>(&self, req: JsonChatRequest) -> Result<T>;
    async fn chat_json_schema<T: DeserializeOwned>(
        &self,
        req: JsonChatRequest,
        schema: &str,
    ) -> Result<T>;
}
