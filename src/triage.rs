use serde::Deserialize;

use crate::error::{Error, Result};

/// Un dossier plausible propose par le LLM pour un mail, avec sa confiance (contrat v2).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Candidate {
    pub category: String,
    pub confidence: f32,
}

/// Verdict brut renvoye par le LLM pour un mail du batch (contrat de triage v2) :
/// les attributs du mail plus la liste ordonnee des dossiers plausibles (`candidates`).
/// Pas de `uid` ici : la reassociation verdict -> mail se fait dans le pipeline
/// (le LLM recoit le batch avec les uid embarques dans le prompt).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LlmVerdict {
    pub priority: i64,
    pub summary: String,
    pub is_newsletter: bool,
    pub candidates: Vec<Candidate>,
}

/// Regle de routage appliquee pour un mail : identifiee dans les logs (une ligne par mail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingRule {
    /// Meilleur candidat retenu (confiance max, a egalite le premier de la liste).
    MeilleurCandidat,
    /// Meilleur candidat sous le seuil configure (ou aucun candidat whitelist) -> INBOX.
    SeuilNonAtteint,
    /// Tous les candidats du verdict sont hors whitelist -> INBOX, avec une regle
    /// de log dediee (issue #28) : distincte de `seuil_non_atteint`, qui garde son
    /// sens de vrai seuil sous la confiance configuree.
    CategorieHorsWhitelistEcartee,
    /// Aucun verdict LLM pour ce uid du batch -> mail laisse en place.
    PasDeVerdict,
    /// Verdict LLM pour un uid inconnu du batch -> ecarte.
    UidInconnuEcarte,
    /// Deux objets verdict pour un meme uid -> candidats fusionnes.
    DoublonFusionne,
}

impl RoutingRule {
    pub fn as_str(self) -> &'static str {
        match self {
            RoutingRule::MeilleurCandidat => "meilleur_candidat",
            RoutingRule::SeuilNonAtteint => "seuil_non_atteint",
            RoutingRule::CategorieHorsWhitelistEcartee => "categorie_hors_whitelist_ecartee",
            RoutingRule::PasDeVerdict => "pas_de_verdict",
            RoutingRule::UidInconnuEcarte => "uid_inconnu_ecarte",
            RoutingRule::DoublonFusionne => "doublon_fusionne",
        }
    }
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
    pub rule: RoutingRule,
}

impl TriageDecision {
    /// Mail laisse en place faute de verdict LLM (contrat v2) : marque comme traite,
    /// aucune destination, aucune action — pas de resoumission en boucle au poll suivant.
    pub fn left_in_place() -> Self {
        Self {
            target_folder: None,
            priority: 5,
            summary: "pas de verdict LLM".into(),
            is_newsletter: false,
            confidence: 0.0,
            action: TriageAction::None,
            rule: RoutingRule::PasDeVerdict,
        }
    }
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

/// Decision finale pour un mail (contrat v2) : whitelist stricte (string-match exact
/// cote Rust, appliquee a chaque candidat), meilleur candidat retenu (confiance maximale,
/// a egalite le premier de la liste), seuil de confiance (sous le seuil -> INBOX),
/// action demandee (copy/move/dry-run).
pub fn decide(
    whitelist: &[String],
    verdict: &LlmVerdict,
    confidence_threshold: f32,
    action: &str,
    dry_run: bool,
) -> Result<TriageDecision> {
    let action = parse_action(action)?;
    let priority = verdict.priority.clamp(1, 5);

    // meilleur candidat de la whitelist : on ne retient que les candidats valides,
    // et on garde le premier en cas d'egalite de confiance (parcours dans l'ordre,
    // remplacement uniquement si strictement superieur).
    let mut best: Option<(&Candidate, f32)> = None;
    for c in &verdict.candidates {
        if !whitelist.iter().any(|f| f == &c.category) {
            continue;
        }
        let conf = clamp01(c.confidence);
        match best {
            None => best = Some((c, conf)),
            Some((_, best_conf)) if conf > best_conf => best = Some((c, conf)),
            _ => {}
        }
    }

    let (target_folder, confidence, rule) = match best {
        Some((c, conf)) if conf >= confidence_threshold => (
            Some(c.category.clone()),
            conf,
            RoutingRule::MeilleurCandidat,
        ),
        Some((_, conf)) => (None, conf, RoutingRule::SeuilNonAtteint),
        // candidats proposes mais tous hors whitelist : regle dediee (issue #28),
        // un vrai seuil sous la confiance reste loggue `seuil_non_atteint`
        None if !verdict.candidates.is_empty() => {
            (None, 0.0, RoutingRule::CategorieHorsWhitelistEcartee)
        }
        None => (None, 0.0, RoutingRule::SeuilNonAtteint),
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
        rule,
    })
}
