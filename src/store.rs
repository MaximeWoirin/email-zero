use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::triage::TriageDecision;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mailbox (
  id TEXT PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS folder (
  mailbox_id TEXT NOT NULL REFERENCES mailbox(id),
  name TEXT NOT NULL,
  description TEXT,
  description_source TEXT NOT NULL DEFAULT 'llm',
  examples TEXT,
  "excluded" INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (mailbox_id, name)
);

CREATE TABLE IF NOT EXISTS processed (
  mailbox_id TEXT NOT NULL REFERENCES mailbox(id),
  folder TEXT NOT NULL,
  uidvalidity INTEGER NOT NULL,
  uid INTEGER NOT NULL,
  processed_at TEXT NOT NULL,
  PRIMARY KEY (mailbox_id, folder, uidvalidity, uid)
);

CREATE TABLE IF NOT EXISTS triage_result (
  mailbox_id TEXT NOT NULL,
  folder TEXT NOT NULL,
  uidvalidity INTEGER NOT NULL,
  uid INTEGER NOT NULL,
  target_folder TEXT,
  priority INTEGER NOT NULL,
  summary TEXT NOT NULL,
  is_newsletter INTEGER NOT NULL,
  confidence REAL NOT NULL,
  action TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (mailbox_id, folder, uidvalidity, uid),
  FOREIGN KEY (mailbox_id, folder, uidvalidity, uid)
    REFERENCES processed (mailbox_id, folder, uidvalidity, uid)
);

CREATE TABLE IF NOT EXISTS discovery_run (
  mailbox_id TEXT NOT NULL REFERENCES mailbox(id),
  started_at TEXT NOT NULL,
  finished_at TEXT,
  status TEXT NOT NULL,
  folders_profiled INTEGER
);
"#;

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TriageRecord {
    pub target_folder: Option<String>,
    pub priority: i64,
    pub summary: String,
    pub is_newsletter: bool,
    pub confidence: f32,
    pub action: String,
}

impl TriageRecord {
    pub fn from_decision(d: &TriageDecision) -> Self {
        Self {
            target_folder: d.target_folder.clone(),
            priority: d.priority,
            summary: d.summary.clone(),
            is_newsletter: d.is_newsletter,
            confidence: d.confidence,
            action: d.action.as_str().to_string(),
        }
    }
}

/// Source de la description d'un dossier : `llm` (profilage automatique par la
/// decouverte) ou `user` (saisie manuelle via `--describe-folder`, que la
/// decouverte ne doit jamais ecraser).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptionSource {
    Llm,
    User,
}

impl DescriptionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DescriptionSource::Llm => "llm",
            DescriptionSource::User => "user",
        }
    }

    /// Toute valeur inconnue retombe sur `llm` (defaut historique).
    pub fn parse(s: &str) -> Self {
        match s {
            "user" => DescriptionSource::User,
            _ => DescriptionSource::Llm,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FolderProfile {
    pub name: String,
    pub description: Option<String>,
    pub description_source: DescriptionSource,
    pub examples: Vec<String>,
    pub updated_at: String,
}

/// Etat complet d'un dossier pour l'affichage (`--list-folders`).
#[derive(Debug, Clone, PartialEq)]
pub struct FolderInfo {
    pub name: String,
    pub description: Option<String>,
    pub description_source: DescriptionSource,
    pub examples: Vec<String>,
    pub excluded: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryRun {
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub folders_profiled: Option<i64>,
}

fn unknown_folder_error(mailbox: &str, folder: &str) -> Error {
    Error::Store(format!(
        "dossier '{folder}' inconnu du state store pour la mailbox '{mailbox}' \
         (lancez d'abord une decouverte des dossiers, ou corrigez le nom : --list-folders)"
    ))
}

/// Secondes since epoch (UTC) maintenant.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_iso8601_utc() -> String {
    iso8601_utc(now_secs())
}

/// Cree le repertoire parent du fichier SQLite s'il n'existe pas (ex: data/).
fn ensure_parent_dir(path: &str) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Conversion secondes-since-epoch -> ISO 8601 UTC ("YYYY-MM-DDTHH:MM:SSZ").
/// Algorithme jours->civil de Howard Hinnant ; teste dans tests/timestamps.rs
/// sur des dates connues verifiees independamment (GNU date).
pub fn iso8601_utc(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    let tod = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod / 60) % 60,
        tod % 60
    )
}

/// Inverse de `iso8601_utc` : parse "YYYY-MM-DDTHH:MM:SSZ" -> secondes since epoch.
/// Algorithme civil->jours de Howard Hinnant (miroir de `iso8601_utc`) ;
/// roundtrip teste dans tests/timestamps.rs. Retourne None si le format est cassé.
pub fn parse_iso8601_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |i: usize, n: usize| -> Option<i64> { s.get(i..i + n)?.parse().ok() };
    let y = num(0, 4)?;
    let m = num(5, 2)?;
    let d = num(8, 2)?;
    let hh = num(11, 2)?;
    let mi = num(14, 2)?;
    let ss = num(17, 2)?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mi > 59 || ss > 60 {
        return None;
    }
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mi * 60 + ss).ok()
}

impl Store {
    pub fn open_in_memory() -> Result<Self> {
        Self::open(":memory:")
    }

    pub fn open(path: &str) -> Result<Self> {
        if path != ":memory:" {
            ensure_parent_dir(path)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // CREATE TABLE IF NOT EXISTS : idempotent, la reouverture d'une DB existante
        // repasse sans erreur (couvert par reouverture_d_une_db_fichier_est_idempotente).
        conn.execute_batch(SCHEMA)?;
        // Migration : la colonne description_source peut manquer sur une DB creee
        // avant son introduction -> ALTER TABLE avec le defaut 'llm' (les
        // descriptions existantes etaient produites par le LLM).
        let has_source: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('folder') WHERE name='description_source'",
            [],
            |row| row.get(0),
        )?;
        if has_source == 0 {
            conn.execute(
                "ALTER TABLE folder ADD COLUMN description_source TEXT NOT NULL DEFAULT 'llm'",
                [],
            )?;
        }
        Ok(Self { conn })
    }

    pub fn sync_mailboxes(&mut self, ids: &[String]) -> Result<()> {
        for id in ids {
            self.conn.execute(
                "INSERT OR IGNORE INTO mailbox (id) VALUES (?1)",
                params![id],
            )?;
        }
        Ok(())
    }

    pub fn mailbox_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT id FROM mailbox ORDER BY id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn is_processed(
        &self,
        mailbox: &str,
        folder: &str,
        uidvalidity: u32,
        uid: u32,
    ) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM processed
             WHERE mailbox_id=?1 AND folder=?2 AND uidvalidity=?3 AND uid=?4",
            params![mailbox, folder, uidvalidity as i64, uid as i64],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn mark_processed(
        &mut self,
        mailbox: &str,
        folder: &str,
        uidvalidity: u32,
        uid: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO processed (mailbox_id, folder, uidvalidity, uid, processed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                mailbox,
                folder,
                uidvalidity as i64,
                uid as i64,
                now_iso8601_utc()
            ],
        )?;
        Ok(())
    }

    /// UIDs de `uids` pas encore traites pour ce (mailbox, folder, uidvalidity), ordre conserve.
    /// N+1 : une requete COUNT par UID. OK a l'echelle d'une boite perso (poll de quelques
    /// mails) ; a rebatir en un seul IN (...) si le backfill historique (v2) arrive.
    pub fn pending_uids(
        &self,
        mailbox: &str,
        folder: &str,
        uidvalidity: u32,
        uids: &[u32],
    ) -> Result<Vec<u32>> {
        let mut pending = Vec::new();
        for &uid in uids {
            if !self.is_processed(mailbox, folder, uidvalidity, uid)? {
                pending.push(uid);
            }
        }
        Ok(pending)
    }

    /// (name, description, excluded) — excluded=1 -> hors whitelist (Sent, Trash...).
    /// La description n'est PAS touchee par le DO UPDATE (revue PR #13) : elle
    /// n'appartient qu'a `set_folder_profile`, pour qu'un run de decouverte qui
    /// echoue mi-parcours ne fasse pas perdre les descriptions du run precedent.
    pub fn upsert_folders(
        &mut self,
        mailbox: &str,
        folders: &[(&str, Option<&str>, bool)],
    ) -> Result<()> {
        let now = now_iso8601_utc();
        for (name, description, excluded) in folders {
            self.conn.execute(
                "INSERT INTO folder (mailbox_id, name, description, \"excluded\", updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (mailbox_id, name) DO UPDATE SET
                   \"excluded\"=excluded.\"excluded\",
                   updated_at=excluded.updated_at",
                params![mailbox, name, description, *excluded as i64, now],
            )?;
        }
        Ok(())
    }

    pub fn whitelist(&self, mailbox: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM folder WHERE mailbox_id=?1 AND \"excluded\"=0 ORDER BY name",
        )?;
        let rows = stmt.query_map(params![mailbox], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// `description = None` = fallback decouverte (LLM indisponible) :
    /// echantillons bruts gardes sans description semantique.
    /// Precedence : une ligne en source `user` (description saisie manuellement)
    /// n'est JAMAIS ecrassee ici — le WHERE l'exclut, la ligne reste intacte.
    pub fn set_folder_profile(
        &mut self,
        mailbox: &str,
        folder: &str,
        description: Option<&str>,
        examples: &[String],
    ) -> Result<()> {
        let examples_json = serde_json::to_string(examples)?;
        self.conn.execute(
            "UPDATE folder SET description=?1, examples=?2, description_source='llm', updated_at=?3
             WHERE mailbox_id=?4 AND name=?5 AND description_source<>'user'",
            params![
                description,
                examples_json,
                now_iso8601_utc(),
                mailbox,
                folder
            ],
        )?;
        Ok(())
    }

    /// Ecrit une description saisie manuellement (source `user`) : la decouverte
    /// ne l'ecrasera plus. Rejette un dossier absent du state store (evite de
    /// creer une entree fantome dans la whitelist via une faute de frappe).
    pub fn set_user_folder_description(
        &mut self,
        mailbox: &str,
        folder: &str,
        description: &str,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE folder SET description=?1, description_source='user', updated_at=?2
             WHERE mailbox_id=?3 AND name=?4",
            params![description, now_iso8601_utc(), mailbox, folder],
        )?;
        if changed == 0 {
            return Err(unknown_folder_error(mailbox, folder));
        }
        Ok(())
    }

    /// Rend la main au profilage LLM : la source repasse a `llm`, la description
    /// courante reste en place jusqu'au prochain run de decouverte.
    pub fn reset_folder_description_source(&mut self, mailbox: &str, folder: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE folder SET description_source='llm', updated_at=?1
             WHERE mailbox_id=?2 AND name=?3",
            params![now_iso8601_utc(), mailbox, folder],
        )?;
        if changed == 0 {
            return Err(unknown_folder_error(mailbox, folder));
        }
        Ok(())
    }

    /// Tous les dossiers d'une boite (y compris exclus), ordre alpha — pour
    /// l'affichage `--list-folders`.
    pub fn folders(&self, mailbox: &str) -> Result<Vec<FolderInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, description, description_source, examples, \"excluded\", updated_at
             FROM folder WHERE mailbox_id=?1 ORDER BY name",
        )?;
        let rows = stmt.query_map(params![mailbox], |row| {
            let examples_json: Option<String> = row.get(3)?;
            let examples = examples_json
                .map(|j| serde_json::from_str::<Vec<String>>(&j).unwrap_or_default())
                .unwrap_or_default();
            Ok(FolderInfo {
                name: row.get(0)?,
                description: row.get(1)?,
                description_source: DescriptionSource::parse(&row.get::<_, String>(2)?),
                examples,
                excluded: row.get::<_, i64>(4)? != 0,
                updated_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Dossiers dont la description est saisie manuellement (source `user`) :
    /// la decouverte les epargne lors du profilage.
    pub fn user_described_folders(&self, mailbox: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM folder WHERE mailbox_id=?1 AND description_source='user' ORDER BY name",
        )?;
        let rows = stmt.query_map(params![mailbox], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn folder_profile(&self, mailbox: &str, folder: &str) -> Result<Option<FolderProfile>> {
        self.conn
            .query_row(
                "SELECT name, description, description_source, examples, updated_at FROM folder
                 WHERE mailbox_id=?1 AND name=?2",
                params![mailbox, folder],
                |row| {
                    let examples_json: Option<String> = row.get(3)?;
                    let examples = examples_json
                        .map(|j| serde_json::from_str::<Vec<String>>(&j).unwrap_or_default())
                        .unwrap_or_default();
                    Ok(FolderProfile {
                        name: row.get(0)?,
                        description: row.get(1)?,
                        description_source: DescriptionSource::parse(&row.get::<_, String>(2)?),
                        examples,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }

    pub fn save_triage_result(
        &mut self,
        mailbox: &str,
        folder: &str,
        uidvalidity: u32,
        uid: u32,
        record: &TriageRecord,
    ) -> Result<()> {
        if !self.is_processed(mailbox, folder, uidvalidity, uid)? {
            return Err(Error::Store(format!(
                "processed absent pour {mailbox}/{folder}/{uidvalidity}/{uid} : triage_result exige la cle processed"
            )));
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO triage_result
               (mailbox_id, folder, uidvalidity, uid, target_folder, priority, summary,
                is_newsletter, confidence, action, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                mailbox,
                folder,
                uidvalidity as i64,
                uid as i64,
                record.target_folder,
                record.priority,
                record.summary,
                record.is_newsletter as i64,
                record.confidence,
                record.action,
                now_iso8601_utc()
            ],
        )?;
        Ok(())
    }

    pub fn triage_result(
        &self,
        mailbox: &str,
        folder: &str,
        uidvalidity: u32,
        uid: u32,
    ) -> Result<Option<TriageRecord>> {
        self.conn
            .query_row(
                "SELECT target_folder, priority, summary, is_newsletter, confidence, action
                 FROM triage_result
                 WHERE mailbox_id=?1 AND folder=?2 AND uidvalidity=?3 AND uid=?4",
                params![mailbox, folder, uidvalidity as i64, uid as i64],
                |row| {
                    Ok(TriageRecord {
                        target_folder: row.get(0)?,
                        priority: row.get(1)?,
                        summary: row.get(2)?,
                        is_newsletter: row.get::<_, i64>(3)? != 0,
                        confidence: row.get(4)?,
                        action: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }

    pub fn start_discovery_run(&mut self, mailbox: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO discovery_run (mailbox_id, started_at, status) VALUES (?1, ?2, 'running')",
            params![mailbox, now_iso8601_utc()],
        )?;
        Ok(())
    }

    pub fn finish_discovery_run(
        &mut self,
        mailbox: &str,
        status: &str,
        folders_profiled: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE discovery_run SET finished_at=?1, status=?2, folders_profiled=?3
             WHERE rowid = (
               SELECT rowid FROM discovery_run
               WHERE mailbox_id=?4 AND finished_at IS NULL
               ORDER BY rowid DESC LIMIT 1
             )",
            params![now_iso8601_utc(), status, folders_profiled, mailbox],
        )?;
        Ok(())
    }

    pub fn discovery_runs(&self, mailbox: &str) -> Result<Vec<DiscoveryRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT started_at, finished_at, status, folders_profiled FROM discovery_run
             WHERE mailbox_id=?1 ORDER BY started_at, rowid",
        )?;
        let rows = stmt.query_map(params![mailbox], |row| {
            Ok(DiscoveryRun {
                started_at: row.get(0)?,
                finished_at: row.get(1)?,
                status: row.get(2)?,
                folders_profiled: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Dernier run de decouverte TERMINE ok/fallback (le plus recent), s'il y en
    /// a un. Les runs `error` ou `running` (orphelin apres crash) sont ignores
    /// (revue PR #13) : seuls les runs reussis font tourner l'horloge
    /// d'intervalle — un echec reste "due", retente au poll suivant.
    /// Sert a `runner::discovery_due`.
    pub fn last_discovery_run(&self, mailbox: &str) -> Result<Option<DiscoveryRun>> {
        self.conn
            .query_row(
                "SELECT started_at, finished_at, status, folders_profiled FROM discovery_run
                 WHERE mailbox_id=?1 AND status IN ('ok','fallback') AND finished_at IS NOT NULL
                 ORDER BY started_at DESC, rowid DESC LIMIT 1",
                params![mailbox],
                |row| {
                    Ok(DiscoveryRun {
                        started_at: row.get(0)?,
                        finished_at: row.get(1)?,
                        status: row.get(2)?,
                        folders_profiled: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }
}
