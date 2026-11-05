use email_zero::triage::{decide, Candidate, LlmVerdict, RoutingRule, TriageAction};

fn whitelist() -> Vec<String> {
    vec!["Factures".into(), "Banque".into(), "Newsletters".into()]
}

fn candidate(category: &str, confidence: f32) -> Candidate {
    Candidate {
        category: category.into(),
        confidence,
    }
}

fn verdict(candidates: Vec<Candidate>) -> LlmVerdict {
    LlmVerdict {
        priority: 3,
        summary: "resume une ligne".into(),
        is_newsletter: false,
        candidates,
    }
}

#[test]
fn meilleur_candidat_choisi_parmi_plusieurs() {
    let v = verdict(vec![
        candidate("Newsletters", 0.85),
        candidate("Factures", 0.95),
        candidate("Banque", 0.9),
    ]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Factures"));
    assert_eq!(d.action, TriageAction::Copy);
    assert_eq!(d.rule, RoutingRule::MeilleurCandidat);
    assert!((d.confidence - 0.95).abs() < f32::EPSILON);
}

#[test]
fn egalite_de_confiance_tri_vers_le_premier_de_la_liste() {
    // contrat v2 : a confiance egale, le premier candidat du tableau gagne
    let v = verdict(vec![candidate("Factures", 0.9), candidate("Banque", 0.9)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Factures"));
    let v = verdict(vec![candidate("Banque", 0.9), candidate("Factures", 0.9)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Banque"));
}

#[test]
fn candidat_hors_whitelist_ignore_et_suivant_valide_retenu() {
    // whitelist stricte cote Rust : un candidat hors whitelist n'est jamais retenu,
    // le meilleur candidat valide de la liste gagne
    let v = verdict(vec![
        candidate("NouveauDossier", 0.99),
        candidate("Factures", 0.9),
    ]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Factures"));
    assert_eq!(d.rule, RoutingRule::MeilleurCandidat);
}

#[test]
fn confiance_sous_seuil_reste_dans_inbox() {
    let v = verdict(vec![candidate("Factures", 0.79)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None);
    assert_eq!(d.action, TriageAction::None);
    assert_eq!(d.rule, RoutingRule::SeuilNonAtteint);
    assert!(
        (d.confidence - 0.79).abs() < f32::EPSILON,
        "la confiance retenue est celle du meilleur candidat meme sous le seuil"
    );
}

#[test]
fn meilleur_candidat_sous_seuil_parmi_plusieurs_reste_dans_inbox() {
    let v = verdict(vec![candidate("Factures", 0.79), candidate("Banque", 0.5)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None);
    assert_eq!(d.rule, RoutingRule::SeuilNonAtteint);
    assert!((d.confidence - 0.79).abs() < f32::EPSILON);
}

#[test]
fn confidence_egale_au_seuil_place_le_mail() {
    let v = verdict(vec![candidate("Factures", 0.8)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Factures"));
    assert_eq!(d.action, TriageAction::Copy);
    assert_eq!(d.rule, RoutingRule::MeilleurCandidat);
}

#[test]
fn category_hors_whitelist_reste_dans_inbox() {
    // validation string-match cote Rust : aucun dossier hors whitelist, jamais de creation
    let v = verdict(vec![candidate("NouveauDossier", 0.99)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None);
    assert_eq!(d.action, TriageAction::None);
    assert_eq!(d.rule, RoutingRule::SeuilNonAtteint);
}

#[test]
fn whitelist_string_match_exact_pas_de_match_partiel() {
    let v = verdict(vec![candidate("Factures ", 0.99)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(
        d.target_folder, None,
        "un espace de plus ne doit pas matcher"
    );
    let v = verdict(vec![candidate("factures", 0.99)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None, "la casse compte");
}

#[test]
fn whitelist_vide_aucun_placement_meme_confiance_max() {
    let v = verdict(vec![candidate("Factures", 1.0)]);
    let d = decide(&[], &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None);
}

#[test]
fn aucun_candidat_reste_dans_inbox() {
    let v = verdict(vec![]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None);
    assert_eq!(d.action, TriageAction::None);
    assert_eq!(d.rule, RoutingRule::SeuilNonAtteint);
    assert!((d.confidence - 0.0).abs() < f32::EPSILON);
}

#[test]
fn action_move_quand_config_move() {
    let v = verdict(vec![candidate("Banque", 0.95)]);
    let d = decide(&whitelist(), &v, 0.8, "move", false).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Banque"));
    assert_eq!(d.action, TriageAction::Move);
}

#[test]
fn dry_run_annonce_l_action_sans_mode_copy_ni_move() {
    let v = verdict(vec![candidate("Banque", 0.95)]);
    let d = decide(&whitelist(), &v, 0.8, "move", true).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Banque"));
    assert_eq!(d.action, TriageAction::DryRun);
}

#[test]
fn dry_run_avec_confiance_sous_seuil_reste_none() {
    let v = verdict(vec![candidate("Banque", 0.5)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", true).unwrap();
    assert_eq!(d.target_folder, None);
    assert_eq!(d.action, TriageAction::None);
}

#[test]
fn action_inconnue_erreur_config() {
    let v = verdict(vec![candidate("Banque", 0.95)]);
    let err = decide(&whitelist(), &v, 0.8, "delete", false).unwrap_err();
    assert!(
        err.to_string().contains("copy") || err.to_string().contains("move"),
        "{err}"
    );
}

#[test]
fn priority_hors_1_5_est_clamp_dans_1_5() {
    let mut v = verdict(vec![candidate("Factures", 0.9)]);
    v.priority = 9;
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.priority, 5);
    let mut v = verdict(vec![candidate("Factures", 0.9)]);
    v.priority = 0;
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.priority, 1);
}

#[test]
fn confiance_du_candidat_hors_0_1_est_clamp() {
    let v = verdict(vec![candidate("Factures", 1.7)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert!((d.confidence - 1.0).abs() < f32::EPSILON);
    assert_eq!(d.target_folder.as_deref(), Some("Factures"));
    let v = verdict(vec![candidate("Factures", -0.2)]);
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert!((d.confidence - 0.0).abs() < f32::EPSILON);
    assert_eq!(
        d.target_folder, None,
        "confiance clampe a 0 < seuil -> INBOX"
    );
}

#[test]
fn les_champs_du_verdict_sont_repris_dans_la_decision() {
    let mut v = verdict(vec![candidate("Newsletters", 0.9)]);
    v.is_newsletter = true;
    v.summary = "promo du mois".into();
    v.priority = 4;
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.summary, "promo du mois");
    assert!(d.is_newsletter);
    assert_eq!(d.priority, 4);
    assert!((d.confidence - 0.9).abs() < f32::EPSILON);
    assert_eq!(d.target_folder.as_deref(), Some("Newsletters"));
}
