use std::collections::HashMap;

use serde::Deserialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::ports::{ChatMessage, JsonChatRequest, LlmClient, MailClient, ParsedMail};
use crate::store::{FolderProfile, Store, TriageRecord};
use crate::triage::{decide, LlmVerdict, TriageAction};

/// JSON schema envoye en structured output : un verdict par mail, reassocie par `uid`.
pub const BATCH_SCHEMA: &str = r#"{
  "type": "json_schema",
  "json_schema": {
    "name": "triage_batch",
    "strict": true,
    "schema": {
      "type": "object",
      "properties": {
        "verdicts": {
          "type": "array",
          "items": {
            "type": "object",
            "properties": {
              "uid": { "type": "integer", "description": "uid du mail dans le batch" },
              "category": { "type": "string", "description": "dossier de la whitelist, jamais un autre" },
              "priority": { "type": "integer", "minimum": 1, "maximum": 5 },
              "summary": { "type": "string", "description": "resume en une ligne" },
              "is_newsletter": { "type": "boolean" },
              "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
            },
            "required": ["uid", "category", "priority", "summary", "is_newsletter", "confidence"],
            "additionalProperties": false
          }
        }
      },
      "required": ["verdicts"],
      "additionalProperties": false
    }
  }
}"#;

/// Verdict du LLM embarquant le `uid` : la reassociation verdict -> mail se fait ici,
/// par uid, dans le moteur de triage (le batch embarque les uid dans le prompt).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UidVerdict {
    pub uid: u64,
    #[serde(flatten)]
    pub verdict: LlmVerdict,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct BatchVerdicts {
    verdicts: Vec<UidVerdict>,
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub whitelist: Vec<String>,
    pub profiles: Vec<FolderProfile>,
    pub confidence_threshold: f32,
    pub action: String,
    pub batch_size: usize,
    pub dry_run: bool,
    pub system_prompt: String,
    pub model: String,
    pub temperature: f32,
    pub top_p: f32,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunStats {
    pub scanned: u64,
    pub pending: u64,
    pub decided: u64,
    pub copied: u64,
    pub moved: u64,
    pub planned: u64,
    pub left_in_inbox: u64,
}

/// Construit le PipelineConfig d'une boite a partir du TOML + du state store :
/// prompt systeme lu depuis `triage.prompt_path`, whitelist et profils depuis `folder`.
pub fn pipeline_config_for(cfg: &Config, store: &Store, mailbox: &str) -> Result<PipelineConfig> {
    let system_prompt = std::fs::read_to_string(&cfg.triage.prompt_path)?;
    let whitelist = store.whitelist(mailbox)?;
    let mut profiles = Vec::with_capacity(whitelist.len());
    for name in &whitelist {
        if let Some(p) = store.folder_profile(mailbox, name)? {
            profiles.push(p);
        }
    }
    Ok(PipelineConfig {
        whitelist,
        profiles,
        confidence_threshold: cfg.triage.confidence_threshold,
        action: cfg.triage.action.clone(),
        batch_size: cfg.llm.batch_size,
        dry_run: false,
        system_prompt,
        model: cfg.llm.model.clone(),
        temperature: cfg.llm.temperature,
        top_p: cfg.llm.top_p,
        max_tokens: cfg.llm.max_tokens,
    })
}

/// Prompt user du batch : whitelist + profil semantique de chaque dossier + mails (uid embarque).
/// Compact et stable d'un poll a l'autre : moins de tokens, decisions coherentes (spec).
pub fn build_user_prompt(
    profiles: &[FolderProfile],
    whitelist: &[String],
    mails: &[(u32, ParsedMail)],
) -> String {
    let mut out = String::from("Dossiers disponibles (whitelist stricte) :\n");
    for name in whitelist {
        match profiles.iter().find(|p| &p.name == name) {
            Some(p) => {
                let desc = p.description.as_deref().unwrap_or("description inconnue");
                let examples = if p.examples.is_empty() {
                    "aucun exemple".to_string()
                } else {
                    p.examples
                        .iter()
                        .map(|e| format!("\"{e}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                out.push_str(&format!("- {name} — {desc} — ex: {examples}\n"));
            }
            None => out.push_str(&format!("- {name}\n")),
        }
    }
    out.push_str("\nEmails a trier (batch) :\n");
    for (uid, mail) in mails {
        out.push_str(&format!(
            "---\nuid={uid}\nfrom: {}\nsubject: {}\nbody: {}\n",
            mail.header.from, mail.header.subject, mail.body_text
        ));
    }
    out
}

/// Un poll de triage sur (mailbox, folder) : SEARCH UNSEEN -> dedup state store (UIDVALIDITY)
/// -> batchs LLM (structured output, reassociation par uid) -> decision pure -> action IMAP
/// (copy/move, rien en dry-run) -> persistance processed + triage_result.
/// Le flag SEEN n'est jamais touche. Toute erreur LLM/IMAP remonte sans marquer le batch.
pub async fn run<M: MailClient, L: LlmClient>(
    mail: &mut M,
    llm: &L,
    store: &mut Store,
    mailbox: &str,
    folder: &str,
    cfg: &PipelineConfig,
) -> Result<RunStats> {
    let mut stats = RunStats::default();
    let uidvalidity = mail.uidvalidity(folder).await?;
    let unseen = mail.search_unseen(folder).await?;
    stats.scanned = unseen.len() as u64;
    let pending = store.pending_uids(mailbox, folder, uidvalidity, &unseen)?;
    stats.pending = pending.len() as u64;
    if pending.is_empty() {
        return Ok(stats);
    }

    for chunk in pending.chunks(cfg.batch_size.max(1)) {
        let mut mails = Vec::with_capacity(chunk.len());
        for &uid in chunk {
            mails.push((uid, mail.fetch_body(folder, uid).await?));
        }
        let req = JsonChatRequest {
            model: cfg.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: cfg.system_prompt.clone(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: build_user_prompt(&cfg.profiles, &cfg.whitelist, &mails),
                },
            ],
            temperature: cfg.temperature,
            top_p: cfg.top_p,
            max_tokens: cfg.max_tokens,
            response_format_json_schema: Some(BATCH_SCHEMA.into()),
        };
        let batch: BatchVerdicts = llm.chat_json_schema(req, BATCH_SCHEMA).await?;

        let mut by_uid: HashMap<u64, LlmVerdict> = HashMap::new();
        for v in &batch.verdicts {
            if by_uid.insert(v.uid, v.verdict.clone()).is_some() {
                return Err(Error::Llm(format!(
                    "uid {} en double dans la reponse LLM",
                    v.uid
                )));
            }
        }
        let mut decisions = Vec::with_capacity(mails.len());
        for (uid, parsed) in &mails {
            let verdict = by_uid.remove(&(*uid as u64)).ok_or_else(|| {
                Error::Llm(format!("pas de verdict LLM pour le uid {uid} du batch"))
            })?;
            let decision = decide(
                &cfg.whitelist,
                &verdict,
                cfg.confidence_threshold,
                &cfg.action,
                cfg.dry_run,
            )?;
            decisions.push((*uid, parsed.clone(), decision));
        }
        if let Some(uid) = by_uid.keys().next() {
            return Err(Error::Llm(format!(
                "verdict orphelin pour le uid {uid}, absent du batch courant"
            )));
        }

        for (uid, _parsed, decision) in decisions {
            match decision.action {
                TriageAction::Copy => {
                    let target = decision.target_folder.clone().unwrap_or_default();
                    mail.copy(folder, uid, &target).await?;
                    stats.copied += 1;
                }
                TriageAction::Move => {
                    let target = decision.target_folder.clone().unwrap_or_default();
                    mail.r#move(folder, uid, &target).await?;
                    stats.moved += 1;
                }
                TriageAction::DryRun => stats.planned += 1,
                TriageAction::None => stats.left_in_inbox += 1,
            }
            store.mark_processed(mailbox, folder, uidvalidity, uid)?;
            store.save_triage_result(
                mailbox,
                folder,
                uidvalidity,
                uid,
                &TriageRecord::from_decision(&decision),
            )?;
            stats.decided += 1;
        }
    }
    Ok(stats)
}
