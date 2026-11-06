//! Commandes de gestion manuelle des dossiers, hors boucle daemon : elles
//! agissent sur le state store seul (pas de connexion IMAP ni LLM) puis quittent.
//!
//! - `--list-folders` : etat des dossiers (source `llm`/`user`, whitelist,
//!   description, exemples, `updated_at`) ;
//! - `--describe-folder <DOSSIER>` : saisit une description en texte libre
//!   (source `user`) — jamais ecrassee par une decouverte ulterieure. Sans
//!   `--describe-examples`, les exemples du profilage LLM sont PURGES (jamais
//!   laisses figes en contradiction avec la description) ;
//! - `--reset-folder-description <DOSSIER>` : rend la main au profilage LLM
//!   (retour a la source `llm`).

use std::io::{Read, Write};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::store::{FolderProfile, Store};

/// Borne de longueur d'une description saisie manuellement (en caracteres) :
/// protege le prompt de triage d'un collage massif.
pub const MAX_DESCRIPTION_CHARS: usize = 2000;

/// Nombre max d'exemples fournis via `--describe-examples`.
pub const MAX_EXAMPLES: usize = 10;

/// Commande dossier demandee (au plus une a la fois).
#[derive(Debug, Clone, PartialEq)]
pub enum FolderCommand {
    List,
    /// `text = None` : la description est lue sur stdin (multiligne, EOF).
    /// `examples = None` : purge des exemples existants ; `Some(liste)` : exemples fournis.
    Describe {
        folder: String,
        text: Option<String>,
        examples: Option<Vec<String>>,
    },
    Reset {
        folder: String,
    },
}

/// Flags CLI de gestion des dossiers, avant validation.
#[derive(Debug, Clone, Default)]
pub struct FolderCli {
    pub list_folders: bool,
    pub describe_folder: Option<String>,
    pub describe_text: Option<String>,
    pub describe_examples: Option<String>,
    pub reset_folder_description: Option<String>,
    pub mailbox: Option<String>,
}

/// Parse `--describe-examples "EX1;EX2;..."` : separateur `;`, trim, vides ignores.
fn parse_examples(raw: &str) -> Result<Vec<String>> {
    let examples: Vec<String> = raw
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if examples.len() > MAX_EXAMPLES {
        return Err(Error::Config(format!(
            "trop d'exemples fournis ({} recus, max {MAX_EXAMPLES})",
            examples.len()
        )));
    }
    Ok(examples)
}

/// Validation d'une description saisie manuellement : trim, non vide, borne de
/// longueur. Retourne la description trimmee.
fn validate_description(text: &str) -> Result<&str> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Config(
            "la description ne peut pas etre vide (texte vide recu)".into(),
        ));
    }
    let len = text.chars().count();
    if len > MAX_DESCRIPTION_CHARS {
        return Err(Error::Config(format!(
            "la description depasse {MAX_DESCRIPTION_CHARS} caracteres ({len} recus)"
        )));
    }
    Ok(text)
}

impl FolderCli {
    /// Validation + construction de la commande ; `Ok(None)` = aucune commande
    /// dossier demandee (le daemon demarre normalement).
    pub fn command(&self) -> Result<Option<FolderCommand>> {
        let actions = [
            self.list_folders,
            self.describe_folder.is_some(),
            self.reset_folder_description.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        if actions > 1 {
            return Err(Error::Config(
                "une seule commande dossier a la fois (--list-folders, --describe-folder, --reset-folder-description)"
                    .into(),
            ));
        }
        if self.describe_folder.is_none()
            && (self.describe_text.is_some() || self.describe_examples.is_some())
        {
            return Err(Error::Config(
                "--describe-text et --describe-examples n'ont de sens qu'avec --describe-folder"
                    .into(),
            ));
        }
        if self.mailbox.is_some() && actions == 0 {
            return Err(Error::Config(
                "--mailbox n'a de sens qu'avec --list-folders, --describe-folder ou --reset-folder-description"
                    .into(),
            ));
        }
        Ok(if self.list_folders {
            Some(FolderCommand::List)
        } else if let Some(folder) = &self.describe_folder {
            Some(FolderCommand::Describe {
                folder: folder.clone(),
                text: self.describe_text.clone(),
                examples: match &self.describe_examples {
                    Some(raw) => Some(parse_examples(raw)?),
                    None => None,
                },
            })
        } else {
            self.reset_folder_description
                .as_ref()
                .map(|folder| FolderCommand::Reset {
                    folder: folder.clone(),
                })
        })
    }
}

/// Boites cibles : `--mailbox` si precisee (doit exister dans la config), sinon
/// toutes les boites configurees.
fn target_mailboxes(cfg: &Config, mailbox: Option<&str>) -> Result<Vec<String>> {
    match mailbox {
        Some(id) => {
            if cfg.mailboxes.iter().any(|m| m.id == id) {
                Ok(vec![id.to_string()])
            } else {
                let known: Vec<&str> = cfg.mailboxes.iter().map(|m| m.id.as_str()).collect();
                Err(Error::Config(format!(
                    "mailbox '{id}' inconnue de la config (connues: {})",
                    known.join(", ")
                )))
            }
        }
        None => Ok(cfg.mailboxes.iter().map(|m| m.id.clone()).collect()),
    }
}

fn unknown_folder(mailbox: &str, folder: &str) -> Error {
    Error::Store(format!(
        "dossier '{folder}' inconnu du state store pour la mailbox '{mailbox}' \
         (lancez d'abord une decouverte des dossiers, ou corrigez le nom : --list-folders)"
    ))
}

/// Pre-verification : le dossier doit exister dans TOUTES les cibles avant toute
/// ecriture (pas d'effet partiel multi-boites).
fn check_all_targets(store: &Store, targets: &[String], folder: &str) -> Result<()> {
    for mb in targets {
        if store.folder_profile(mb, folder)?.is_none() {
            return Err(unknown_folder(mb, folder));
        }
    }
    Ok(())
}

fn write_examples(out: &mut dyn Write, examples: &[String]) -> Result<()> {
    if examples.is_empty() {
        writeln!(out, "    exemples : aucun")?;
    } else {
        let list = examples
            .iter()
            .map(|e| format!("\"{e}\""))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(out, "    exemples ({}) : {list}", examples.len())?;
    }
    Ok(())
}

/// Affiche l'etat actuel d'un dossier (source, description, exemples).
fn show_folder(
    store: &Store,
    mailbox: &str,
    folder: &str,
    out: &mut dyn Write,
) -> Result<FolderProfile> {
    let before = store
        .folder_profile(mailbox, folder)?
        .ok_or_else(|| unknown_folder(mailbox, folder))?;
    writeln!(out, "== {mailbox} / {folder} ==")?;
    writeln!(
        out,
        "  description actuelle [source={}] : {}",
        before.description_source.as_str(),
        before.description.as_deref().unwrap_or("(aucune)")
    )?;
    write_examples(out, &before.examples)?;
    Ok(before)
}

/// `--list-folders` : etat des dossiers d'une boite.
pub fn list_folders(store: &Store, mailbox: &str, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "== {mailbox} ==")?;
    let folders = store.folders(mailbox)?;
    if folders.is_empty() {
        writeln!(
            out,
            "  (aucun dossier dans le state store — la decouverte des dossiers n'a pas encore tourne)"
        )?;
        return Ok(());
    }
    for f in &folders {
        let status = if f.excluded {
            "hors whitelist"
        } else {
            "whitelist"
        };
        writeln!(
            out,
            "  {} [source={}] [{}] maj={}",
            f.name,
            f.description_source.as_str(),
            status,
            f.updated_at
        )?;
        match &f.description {
            Some(d) => writeln!(out, "    description : {d}")?,
            None => writeln!(out, "    description : (aucune)")?,
        }
        write_examples(out, &f.examples)?;
    }
    Ok(())
}

/// `--describe-folder` : enregistre la description saisie manuellement (source
/// `user`) et confirme. `examples = None` purge les exemples existants.
/// L'etat actuel est affiche par l'appelant (`show_folder`) avant la saisie.
pub fn describe_folder(
    store: &mut Store,
    mailbox: &str,
    folder: &str,
    text: &str,
    examples: Option<&[String]>,
    out: &mut dyn Write,
) -> Result<()> {
    let text = validate_description(text)?;
    let examples: &[String] = examples.unwrap_or(&[]);
    store.set_user_folder_description(mailbox, folder, text, examples)?;
    writeln!(out, "  nouvelle description [source=user] : {text}")?;
    write_examples(out, examples)?;
    writeln!(out, "  elle ne sera jamais ecrassee par une decouverte ulterieure (--refresh-folders compris) ; --reset-folder-description '{folder}' rend la main au profilage LLM")?;
    Ok(())
}

/// `--reset-folder-description` : la source de la description repasse a `llm` ;
/// la prochaine decouverte re-profilera le dossier.
pub fn reset_folder_description(
    store: &mut Store,
    mailbox: &str,
    folder: &str,
    out: &mut dyn Write,
) -> Result<()> {
    let before = store
        .folder_profile(mailbox, folder)?
        .ok_or_else(|| unknown_folder(mailbox, folder))?;
    store.reset_folder_description_source(mailbox, folder)?;
    writeln!(
        out,
        "== {mailbox} / {folder} == source de la description : '{}' -> 'llm'",
        before.description_source.as_str()
    )?;
    writeln!(
        out,
        "  la prochaine decouverte (intervalle ou --refresh-folders) re-profilera le dossier (description ET exemples)"
    )?;
    Ok(())
}

/// Point d'entree : execute la commande dossier sur chaque boite cible.
pub fn run_folder_command(
    cfg: &Config,
    store: &mut Store,
    cmd: &FolderCommand,
    mailbox: Option<&str>,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<()> {
    let targets = target_mailboxes(cfg, mailbox)?;
    match cmd {
        FolderCommand::List => {
            for mb in &targets {
                list_folders(store, mb, out)?;
            }
        }
        FolderCommand::Describe {
            folder,
            text,
            examples,
        } => {
            // pre-verification sur toutes les cibles avant toute ecriture
            check_all_targets(store, &targets, folder)?;
            // etat actuel affiche avant la saisie (utile en mode stdin interactif)
            for mb in &targets {
                show_folder(store, mb, folder, out)?;
            }
            let text = match text {
                Some(t) => t.clone(),
                None => {
                    writeln!(
                        out,
                        ">>> description attendue sur stdin (multiligne, EOF / Ctrl-D pour terminer)"
                    )?;
                    let mut buf = String::new();
                    stdin.read_to_string(&mut buf)?;
                    buf
                }
            };
            // validation globale avant la moindre ecriture
            let text = validate_description(&text)?.to_string();
            for mb in &targets {
                describe_folder(store, mb, folder, &text, examples.as_deref(), out)?;
            }
        }
        FolderCommand::Reset { folder } => {
            check_all_targets(store, &targets, folder)?;
            for mb in &targets {
                reset_folder_description(store, mb, folder, out)?;
            }
        }
    }
    Ok(())
}
