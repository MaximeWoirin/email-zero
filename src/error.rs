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
}

pub type Result<T> = std::result::Result<T, Error>;
