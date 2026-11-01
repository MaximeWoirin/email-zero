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

#[derive(Debug, Clone, PartialEq)]
pub struct FolderProfile {
    pub name: String,
    pub description: Option<String>,
    pub examples: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryRun {
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub folders_profiled: Option<i64>,
}

fn now_iso8601_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let z = secs as i64 + 719_468;
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

impl Store {
    pub fn open_in_memory() -> Result<Self> {
        Self::open(":memory:")
    }

    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
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
                   description=excluded.description,
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

    pub fn set_folder_profile(
        &mut self,
        mailbox: &str,
        folder: &str,
        description: &str,
        examples: &[String],
    ) -> Result<()> {
        let examples_json = serde_json::to_string(examples)?;
        self.conn.execute(
            "UPDATE folder SET description=?1, examples=?2, updated_at=?3
             WHERE mailbox_id=?4 AND name=?5",
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

    pub fn folder_profile(&self, mailbox: &str, folder: &str) -> Result<Option<FolderProfile>> {
        self.conn
            .query_row(
                "SELECT name, description, examples, updated_at FROM folder
                 WHERE mailbox_id=?1 AND name=?2",
                params![mailbox, folder],
                |row| {
                    let examples_json: Option<String> = row.get(2)?;
                    let examples = examples_json
                        .map(|j| serde_json::from_str::<Vec<String>>(&j).unwrap_or_default())
                        .unwrap_or_default();
                    Ok(FolderProfile {
                        name: row.get(0)?,
                        description: row.get(1)?,
                        examples,
                        updated_at: row.get(3)?,
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
}
