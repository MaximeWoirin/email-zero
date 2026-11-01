use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(rename = "mailbox")]
    pub mailboxes: Vec<MailboxConfig>,
    pub llm: LlmConfig,
    #[serde(rename = "imap", default)]
    pub imap_retry: ImapSection,
    #[serde(default)]
    pub triage: TriageConfig,
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    #[serde(default)]
    pub store: StoreSection,
    #[serde(default)]
    pub log: LogConfig,
}

#[derive(Clone, Deserialize)]
pub struct MailboxConfig {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password_env: Option<String>,
    pub password_cmd: Option<String>,
    #[serde(rename = "password")]
    pub password_plain: Option<String>,
    #[serde(default)]
    pub tls: bool,
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
}

impl std::fmt::Debug for MailboxConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailboxConfig")
            .field("id", &self.id)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password_env", &self.password_env.as_ref().map(|_| "***"))
            .field("password_cmd", &self.password_cmd.as_ref().map(|_| "***"))
            .field(
                "password_plain",
                &self.password_plain.as_ref().map(|_| "***"),
            )
            .field("tls", &self.tls)
            .field("poll_interval_secs", &self.poll_interval_secs)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key_env: String,
    pub model: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_top_p")]
    pub top_p: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    pub reasoning_effort: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    #[serde(default)]
    pub rate_limit_per_min: u32,
    #[serde(default)]
    pub retry: RetryConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImapSection {
    #[serde(default)]
    pub retry: RetryConfig,
}

impl Default for ImapSection {
    fn default() -> Self {
        Self {
            retry: RetryConfig {
                max_attempts: default_max_attempts(),
                backoff_base_secs: 2,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RetryConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_backoff_base_secs")]
    pub backoff_base_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TriageConfig {
    #[serde(default = "default_action")]
    pub action: String,
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
    #[serde(default = "default_prompt_path")]
    pub prompt_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryConfig {
    #[serde(default = "default_discovery_interval_secs")]
    pub interval_secs: u64,
    #[serde(default = "default_sample_per_folder")]
    pub sample_per_folder: usize,
    #[serde(default = "default_examples_in_prompt")]
    pub examples_in_prompt: usize,
    #[serde(default = "default_exclude_folders")]
    pub exclude_folders: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreSection {
    #[serde(default = "default_store_path")]
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Config {
    pub fn parse(toml_str: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(toml_str)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn from_file(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    fn validate(&self) -> Result<()> {
        if self.mailboxes.is_empty() {
            return Err(Error::Config("au moins une [[mailbox]] est requise".into()));
        }
        for mb in &self.mailboxes {
            if self.mailboxes.iter().filter(|m| m.id == mb.id).count() > 1 {
                return Err(Error::Config(format!("id de mailbox en double: {}", mb.id)));
            }
            mb.validate()?;
        }
        if !matches!(self.triage.action.as_str(), "copy" | "move") {
            return Err(Error::Config(format!(
                "triage.action doit etre \"copy\" ou \"move\", recu: {}",
                self.triage.action
            )));
        }
        if !(0.0..=1.0).contains(&self.triage.confidence_threshold) {
            return Err(Error::Config(format!(
                "triage.confidence_threshold doit etre dans [0, 1], recu: {}",
                self.triage.confidence_threshold
            )));
        }
        Ok(())
    }

    pub fn plaintext_password_mailboxes(&self) -> Vec<&str> {
        self.mailboxes
            .iter()
            .filter(|m| m.uses_plaintext_password())
            .map(|m| m.id.as_str())
            .collect()
    }
}

impl MailboxConfig {
    fn validate(&self) -> Result<()> {
        let sources = [
            self.password_env.is_some(),
            self.password_cmd.is_some(),
            self.password_plain.is_some(),
        ];
        if sources.iter().filter(|s| **s).count() != 1 {
            return Err(Error::Config(format!(
                "mailbox {}: exactement une source de secret requise (password_env, password_cmd ou password)",
                self.id
            )));
        }
        Ok(())
    }

    pub fn uses_plaintext_password(&self) -> bool {
        self.password_plain.is_some()
    }

    pub fn resolve_password(&self, env_lookup: &dyn Fn(&str) -> Option<String>) -> Result<String> {
        if let Some(env) = &self.password_env {
            return env_lookup(env).ok_or_else(|| Error::MissingSecretEnv(env.clone()));
        }
        if let Some(cmd) = &self.password_cmd {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .map_err(|e| {
                    Error::Config(format!(
                        "mailbox {}: password_cmd {cmd:?} injoignable: {e}",
                        self.id
                    ))
                })?;
            if !output.status.success() {
                return Err(Error::Config(format!(
                    "mailbox {}: password_cmd {cmd:?} a echoue (code {:?})",
                    self.id,
                    output.status.code()
                )));
            }
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        if let Some(plain) = &self.password_plain {
            return Ok(plain.clone());
        }
        Err(Error::Config(format!(
            "mailbox {}: aucune source de secret definie",
            self.id
        )))
    }

    pub fn resolve_password_from_env(&self) -> Result<String> {
        self.resolve_password(&|name| std::env::var(name).ok())
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            backoff_base_secs: default_backoff_base_secs(),
        }
    }
}

impl Default for TriageConfig {
    fn default() -> Self {
        Self {
            action: default_action(),
            confidence_threshold: default_confidence_threshold(),
            prompt_path: default_prompt_path(),
        }
    }
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_discovery_interval_secs(),
            sample_per_folder: default_sample_per_folder(),
            examples_in_prompt: default_examples_in_prompt(),
            exclude_folders: default_exclude_folders(),
        }
    }
}

impl Default for StoreSection {
    fn default() -> Self {
        Self {
            path: default_store_path(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

fn default_poll_interval_secs() -> u64 {
    300
}
fn default_temperature() -> f32 {
    0.2
}
fn default_top_p() -> f32 {
    1.0
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_timeout_secs() -> u64 {
    90
}
fn default_batch_size() -> usize {
    15
}
fn default_max_attempts() -> u32 {
    3
}
fn default_backoff_base_secs() -> u64 {
    5
}
fn default_action() -> String {
    "copy".into()
}
fn default_confidence_threshold() -> f32 {
    0.8
}
fn default_prompt_path() -> String {
    "prompts/triage.md".into()
}
fn default_discovery_interval_secs() -> u64 {
    86400
}
fn default_sample_per_folder() -> usize {
    20
}
fn default_examples_in_prompt() -> usize {
    3
}
fn default_exclude_folders() -> Vec<String> {
    ["Sent", "Drafts", "Trash", "Junk"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}
fn default_store_path() -> String {
    "data/email-zero.sqlite".into()
}
fn default_log_level() -> String {
    "info".into()
}
