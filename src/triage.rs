use serde::Deserialize;

use crate::error::{Error, Result};

/// Verdict brut renvoye par le LLM pour un mail du batch (deserialise depuis le JSON strict).
/// Pas de `uid` ici : la reassociation verdict -> mail se fera dans le moteur de triage
/// (le LLM recoit le batch avec les uid embarques dans le prompt, et la reponse est
/// reassociee par position/uid a ce moment-la).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LlmVerdict {
    pub category: String,
    pub priority: i64,
    pub summary: String,
    pub is_newsletter: bool,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageAction {
    None,
    Copy,
    Move,
    DryRun,
}

impl TriageAction {
    pub fn as_str(self) -> &'static str {
        match self {
            TriageAction::None => "none",
            TriageAction::Copy => "copy",
            TriageAction::Move => "move",
            TriageAction::DryRun => "dry_run",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TriageDecision {
    pub target_folder: Option<String>,
    pub priority: i64,
    pub summary: String,
    pub is_newsletter: bool,
    pub confidence: f32,
    pub action: TriageAction,
}

fn parse_action(action: &str) -> Result<TriageAction> {
    match action {
        "copy" => Ok(TriageAction::Copy),
        "move" => Ok(TriageAction::Move),
        other => Err(Error::Config(format!(
            "triage.action doit etre \"copy\" ou \"move\", recu: {other}"
        ))),
    }
}

fn clamp01(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// Decision finale pour un mail : whitelist stricte (string-match exact cote Rust),
/// seuil de confiance (sous le seuil -> INBOX), action demandee (copy/move/dry-run).
pub fn decide(
    whitelist: &[String],
    verdict: &LlmVerdict,
    confidence_threshold: f32,
    action: &str,
    dry_run: bool,
) -> Result<TriageDecision> {
    let action = parse_action(action)?;
    let confidence = clamp01(verdict.confidence);
    let priority = verdict.priority.clamp(1, 5);

    let in_whitelist = whitelist.iter().any(|f| f == &verdict.category);
    let target_folder = if in_whitelist && confidence >= confidence_threshold {
        Some(verdict.category.clone())
    } else {
        None
    };

    let action = match (target_folder.is_some(), dry_run) {
        (true, true) => TriageAction::DryRun,
        (true, false) => action,
        (false, _) => TriageAction::None,
    };

    Ok(TriageDecision {
        target_folder,
        priority,
        summary: verdict.summary.clone(),
        is_newsletter: verdict.is_newsletter,
        confidence,
        action,
    })
}
