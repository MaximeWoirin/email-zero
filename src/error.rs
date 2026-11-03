use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config invalide: {0}")]
    Config(String),
    #[error("secret introuvable: variable d'environnement {0} non definie")]
    MissingSecretEnv(String),
    #[error("lecture config: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing config TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("state store: {0}")]
    Store(String),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("llm: {0}")]
    Llm(String),
    /// Erreur LLM definitive (ex: HTTP 4xx hors 429) : `with_retry` ne retente pas.
    #[error("llm (non retentable): {0}")]
    LlmNonRetryable(String),
    #[error("imap: {0}")]
    Imap(String),
}

pub type Result<T> = std::result::Result<T, Error>;
