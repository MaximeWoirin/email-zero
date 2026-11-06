use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::ports::{ChatMessage, Folder, JsonChatRequest, LlmClient, MailClient};
use crate::store::Store;

/// JSON schema du profilage (structured output) : un profil par dossier,
/// reassocie par `name` (jamais par l'ordre de la reponse).
pub const PROFILING_SCHEMA: &str = r#"{
  "type": "json_schema",
  "json_schema": {
    "name": "folder_profiles",
    "strict": true,
    "schema": {
      "type": "object",
      "properties": {
        "folders": {
          "type": "array",
          "items": {
            "type": "object",
            "properties": {
              "name": { "type": "string", "description": "nom du dossier, tel que fourni" },
              "description": { "type": "string", "description": "description en une ligne" },
              "examples": {
                "type": "array",
                "items": { "type": "string" },
                "description": "2-3 exemples de sujets typiques"
              }
            },
            "required": ["name", "description", "examples"],
            "additionalProperties": false
          }
        }
      },
      "required": ["folders"],
      "additionalProperties": false
    }
  }
}"#;

/// Description reservee aux dossiers vides (le LLM ne les verra jamais).
pub const UNKNOWN_FOLDER_DESCRIPTION: &str = "inconnue";

/// INBOX est le dossier source du tri, jamais une cible : hors whitelist.
fn is_source_folder(name: &str) -> bool {
    name.eq_ignore_ascii_case("INBOX")
}

/// Exclusion d'un dossier : un composant de son chemin (separateur `/`) OU son
/// special-use (RFC 6154) correspond, sans le backslash initial et insensible a
/// la casse, a une entree de `exclude_folders` — un dossier exclu exclut tout
/// son sous-arbre (matching par prefixe : `Trash` exclut `INBOX/Trash` comme
/// `INBOX/TRASH/Documents`, issue #28). Le dossier source INBOX est toujours
/// exclu. `\Noselect` est deja filtre par le client IMAP.
pub fn is_excluded(folder: &Folder, exclude_folders: &[String]) -> bool {
    if is_source_folder(&folder.name) {
        return true;
    }
    let matches = |value: &str| {
        let v = value.trim_start_matches('\\');
        exclude_folders
            .iter()
            .any(|e| e.trim_start_matches('\\').eq_ignore_ascii_case(v))
    };
    if folder
        .name
        .split('/')
        .filter(|c| !c.is_empty())
        .any(matches)
    {
        return true;
    }
    folder.special_use.as_deref().map(matches).unwrap_or(false)
}

/// Parametres du job de decouverte, construits depuis le TOML + le prompt fichier.
#[derive(Debug, Clone)]
pub struct DiscoveryParams {
    pub sample_per_folder: usize,
    pub exclude_folders: Vec<String>,
    pub system_prompt: String,
    pub model: String,
    pub temperature: f32,
    pub top_p: f32,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryStats {
    pub folders_seen: u64,
    pub whitelisted: u64,
    pub sampled: u64,
    /// dossiers ayant recu une description du LLM (0 en fallback)
    pub profiled: u64,
    /// "ok" | "fallback" (LLM indisponible)
    pub status: &'static str,
}

/// Echantillons d'un dossier : expediteurs les plus frequents + (from, subject)
/// des N derniers mails.
#[derive(Debug, Clone)]
pub struct FolderSample {
    pub name: String,
    pub top_senders: Vec<String>,
    pub mails: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct FolderProfileOut {
    name: String,
    description: String,
    examples: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct ProfilingResponse {
    folders: Vec<FolderProfileOut>,
}

/// Construit les parametres de decouverte depuis la config (prompt systeme lu
/// depuis `discovery.prompt_path`, comme `triage.prompt_path`).
pub fn discovery_params_for(cfg: &Config) -> Result<DiscoveryParams> {
    let system_prompt = std::fs::read_to_string(&cfg.discovery.prompt_path)?;
    Ok(DiscoveryParams {
        sample_per_folder: cfg.discovery.sample_per_folder,
        exclude_folders: cfg.discovery.exclude_folders.clone(),
        system_prompt,
        model: cfg.llm.model.clone(),
        temperature: cfg.llm.temperature,
        top_p: cfg.llm.top_p,
        max_tokens: cfg.llm.max_tokens,
    })
}

/// Prompt user du profilage : par dossier, expediteurs frequents + sujets des
/// derniers mails. Envoie uniquement pour les dossiers non vides.
pub fn build_profiling_prompt(samples: &[FolderSample]) -> String {
    let mut out = String::from("Dossiers de la boite et echantillons de leurs derniers mails :\n");
    for s in samples {
        out.push_str(&format!("\n- {}\n", s.name));
        if !s.top_senders.is_empty() {
            out.push_str(&format!(
                "  expediteurs frequents : {}\n",
                s.top_senders.join(", ")
            ));
        }
        for (from, subject) in &s.mails {
            out.push_str(&format!("  - \"{subject}\" ({from})\n"));
        }
    }
    out.push_str("\nReponds avec le JSON demande : un profil (name, description, examples) par dossier de cette liste.\n");
    out
}

/// Expediteurs les plus frequents (ordre decroissant, egale a l'alpha), borne a 5.
fn top_senders(mails: &[(String, String)]) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (from, _) in mails {
        *counts.entry(from.as_str()).or_default() += 1;
    }
    let mut v: Vec<(&str, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter().take(5).map(|(s, _)| s.to_string()).collect()
}

/// Echantillonne un dossier : les `sample_per_folder` derniers UIDs (SEARCH ALL,
/// lus ou non) -> headers via BODY.PEEK[HEADER] (jamais de \Seen).
async fn sample_folder<M: MailClient>(
    mail: &mut M,
    folder: &str,
    sample_per_folder: usize,
) -> Result<FolderSample> {
    if sample_per_folder == 0 {
        // convention projet « 0 = desactive » : aucun echantillonnage, aucune
        // commande IMAP — le dossier est traite comme vide pour le prompt.
        return Ok(FolderSample {
            name: folder.to_string(),
            top_senders: Vec::new(),
            mails: Vec::new(),
        });
    }
    let mut uids = mail.search_all(folder).await?;
    uids.sort_unstable();
    let mut last: Vec<u32> = uids.iter().rev().take(sample_per_folder).copied().collect();
    last.sort_unstable();
    let mut mails = Vec::with_capacity(last.len());
    for uid in last {
        let h = mail.fetch_headers(folder, uid).await?;
        mails.push((h.from, h.subject));
    }
    let top_senders = top_senders(&mails);
    Ok(FolderSample {
        name: folder.to_string(),
        top_senders,
        mails,
    })
}

async fn run_discovery_inner<M: MailClient, L: LlmClient>(
    mail: &mut M,
    llm: &L,
    store: &mut Store,
    mailbox: &str,
    params: &DiscoveryParams,
) -> Result<DiscoveryStats> {
    let folders = mail.list_folders().await?;
    let seen = folders.len() as u64;

    let mut rows = Vec::with_capacity(folders.len());
    for f in &folders {
        rows.push((
            f.name.as_str(),
            None,
            is_excluded(f, &params.exclude_folders),
        ));
    }
    store.upsert_folders(mailbox, &rows)?;
    let whitelisted = rows.iter().filter(|(_, _, ex)| !*ex).count() as u64;

    // Dossiers dont la description est saisie manuellement (source `user`) :
    // epargnes par le profilage — ni echantillonnes, ni envoyes au LLM, leur
    // ligne `folder` (description, exemples, source) reste intacte.
    let user_folders: HashSet<String> =
        store.user_described_folders(mailbox)?.into_iter().collect();
    if !user_folders.is_empty() {
        tracing::info!(
            mailbox,
            preserved = user_folders.len(),
            dossiers = ?user_folders,
            "dossiers a description manuelle (source user) epargnes par le profilage"
        );
    }

    let mut samples = Vec::new();
    for f in &folders {
        if is_excluded(f, &params.exclude_folders) || user_folders.contains(&f.name) {
            continue;
        }
        samples.push(sample_folder(mail, &f.name, params.sample_per_folder).await?);
    }
    let sampled = samples.iter().filter(|s| !s.mails.is_empty()).count() as u64;

    // Profilage LLM : UNE seule requete pour tous les dossiers non vides de la
    // boite. Reponse reassociee par `name`.
    let non_empty: Vec<&FolderSample> = samples.iter().filter(|s| !s.mails.is_empty()).collect();
    let mut profiles: HashMap<String, FolderProfileOut> = HashMap::new();
    let mut status = "ok";
    if !non_empty.is_empty() {
        let req = JsonChatRequest {
            model: params.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: params.system_prompt.clone(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: build_profiling_prompt(
                        &non_empty.iter().map(|s| (*s).clone()).collect::<Vec<_>>(),
                    ),
                },
            ],
            temperature: params.temperature,
            top_p: params.top_p,
            max_tokens: params.max_tokens,
            response_format_json_schema: Some(PROFILING_SCHEMA.into()),
        };
        match llm
            .chat_json_schema::<ProfilingResponse>(req, PROFILING_SCHEMA)
            .await
        {
            Ok(resp) => {
                let mut by_name: HashMap<String, FolderProfileOut> = HashMap::new();
                for p in resp.folders {
                    let name = p.name.clone();
                    if by_name.insert(name.clone(), p).is_some() {
                        return Err(Error::Llm(format!(
                            "profil en double pour le dossier {name}"
                        )));
                    }
                }
                for s in &non_empty {
                    let p = by_name.remove(&s.name).ok_or_else(|| {
                        Error::Llm(format!("pas de profil LLM pour le dossier {}", s.name))
                    })?;
                    profiles.insert(s.name.clone(), p);
                }
                if let Some(orphan) = by_name.keys().next() {
                    return Err(Error::Llm(format!(
                        "profil orphelin pour le dossier {orphan}, absent de la requete"
                    )));
                }
            }
            Err(e) => {
                // LLM indisponible a la decouverte : fallback sur les
                // echantillons bruts, sans description (spec).
                tracing::warn!(mailbox, error = %e, "LLM indisponible au profilage, fallback echantillons bruts");
                status = "fallback";
            }
        }
    }

    // Persistance des profils dans `folder` (description + exemples + updated_at).
    // Les dossiers en source `user` sont sautes : set_folder_profile les protegerait
    // de toute facon, mais ne pas les toucher du tout preserve aussi updated_at.
    for f in &folders {
        if is_excluded(f, &params.exclude_folders) || user_folders.contains(&f.name) {
            continue;
        }
        let sample = samples.iter().find(|s| s.name == f.name);
        match (sample, profiles.get(&f.name)) {
            (Some(s), Some(p)) if !s.mails.is_empty() => {
                store.set_folder_profile(mailbox, &f.name, Some(&p.description), &p.examples)?;
            }
            (Some(s), None) if !s.mails.is_empty() => {
                // fallback : echantillons bruts (sujets) sans description
                let subjects: Vec<String> = s.mails.iter().map(|(_, subj)| subj.clone()).collect();
                store.set_folder_profile(mailbox, &f.name, None, &subjects)?;
            }
            _ => {
                // dossier vide : description "inconnue", jamais envoye au LLM
                store.set_folder_profile(
                    mailbox,
                    &f.name,
                    Some(UNKNOWN_FOLDER_DESCRIPTION),
                    &[],
                )?;
            }
        }
    }

    Ok(DiscoveryStats {
        folders_seen: seen,
        whitelisted,
        sampled,
        profiled: profiles.len() as u64,
        status,
    })
}

/// Job de decouverte des dossiers d'une boite (jalon 2c) : LIST + special-use
/// -> whitelist dans `folder` ; echantillonnage des N derniers headers par
/// dossier -> UNE requete LLM de profilage -> profils en cache ; historique
/// `discovery_run` (ok | fallback | error).
///
/// LLM indisponible -> statut "fallback", echantillons bruts gardes sans
/// description, la whitelist reste utilisable. Erreur IMAP -> statut "error"
/// et erreur propagee (le daemon loggue et retente au tour suivant).
pub async fn run_discovery<M: MailClient, L: LlmClient>(
    mail: &mut M,
    llm: &L,
    store: &mut Store,
    mailbox: &str,
    params: &DiscoveryParams,
) -> Result<DiscoveryStats> {
    store.start_discovery_run(mailbox)?;
    match run_discovery_inner(mail, llm, store, mailbox, params).await {
        Ok(stats) => {
            // revue PR #13 : folders_profiled = nombre de profils LLM effectifs
            // (les dossiers vides « inconnue » ne comptent pas).
            let profiled_count = if stats.status == "ok" {
                Some(stats.profiled as i64)
            } else {
                None
            };
            store.finish_discovery_run(mailbox, stats.status, profiled_count)?;
            Ok(stats)
        }
        Err(e) => {
            let _ = store.finish_discovery_run(mailbox, "error", None);
            Err(e)
        }
    }
}
