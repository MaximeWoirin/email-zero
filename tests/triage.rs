use email_zero::triage::{decide, LlmVerdict, TriageAction};

fn whitelist() -> Vec<String> {
    vec!["Factures".into(), "Banque".into(), "Newsletters".into()]
}

fn verdict(category: &str, confidence: f32) -> LlmVerdict {
    LlmVerdict {
        category: category.into(),
        priority: 3,
        summary: "resume une ligne".into(),
        is_newsletter: false,
        confidence,
    }
}

#[test]
fn confidence_sous_seuil_reste_dans_inbox() {
    let d = decide(&whitelist(), &verdict("Factures", 0.79), 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None);
    assert_eq!(d.action, TriageAction::None);
}

#[test]
fn confidence_egale_au_seuil_place_le_mail() {
    let d = decide(&whitelist(), &verdict("Factures", 0.8), 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Factures"));
    assert_eq!(d.action, TriageAction::Copy);
}

#[test]
fn category_hors_whitelist_reste_dans_inbox() {
    // validation string-match cote Rust : aucun dossier hors whitelist, jamais de creation
    let d = decide(
        &whitelist(),
        &verdict("NouveauDossier", 0.99),
        0.8,
        "copy",
        false,
    )
    .unwrap();
    assert_eq!(d.target_folder, None);
    assert_eq!(d.action, TriageAction::None);
}

#[test]
fn whitelist_string_match_exact_pas_de_match_partiel() {
    let d = decide(
        &whitelist(),
        &verdict("Factures ", 0.99),
        0.8,
        "copy",
        false,
    )
    .unwrap();
    assert_eq!(
        d.target_folder, None,
        "un espace de plus ne doit pas matcher"
    );
    let d = decide(&whitelist(), &verdict("factures", 0.99), 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None, "la casse compte");
}

#[test]
fn whitelist_vide_aucun_placement_meme_confiance_max() {
    let d = decide(&[], &verdict("Factures", 1.0), 0.8, "copy", false).unwrap();
    assert_eq!(d.target_folder, None);
}

#[test]
fn action_move_quand_config_move() {
    let d = decide(&whitelist(), &verdict("Banque", 0.95), 0.8, "move", false).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Banque"));
    assert_eq!(d.action, TriageAction::Move);
}

#[test]
fn dry_run_annonce_l_action_sans_mode_copy_ni_move() {
    let d = decide(&whitelist(), &verdict("Banque", 0.95), 0.8, "move", true).unwrap();
    assert_eq!(d.target_folder.as_deref(), Some("Banque"));
    assert_eq!(d.action, TriageAction::DryRun);
}

#[test]
fn dry_run_avec_confiance_sous_seuil_reste_none() {
    let d = decide(&whitelist(), &verdict("Banque", 0.5), 0.8, "copy", true).unwrap();
    assert_eq!(d.target_folder, None);
    assert_eq!(d.action, TriageAction::None);
}

#[test]
fn action_inconnue_erreur_config() {
    let err = decide(&whitelist(), &verdict("Banque", 0.95), 0.8, "delete", false).unwrap_err();
    assert!(
        err.to_string().contains("copy") || err.to_string().contains("move"),
        "{err}"
    );
}

#[test]
fn priority_hors_1_5_est_clamp_dans_1_5() {
    let mut v = verdict("Factures", 0.9);
    v.priority = 9;
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.priority, 5);
    let mut v = verdict("Factures", 0.9);
    v.priority = 0;
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.priority, 1);
}

#[test]
fn confidence_hors_0_1_est_clamp() {
    let d = decide(&whitelist(), &verdict("Factures", 1.7), 0.8, "copy", false).unwrap();
    assert!((d.confidence - 1.0).abs() < f32::EPSILON);
    let d = decide(&whitelist(), &verdict("Factures", -0.2), 0.8, "copy", false).unwrap();
    assert!((d.confidence - 0.0).abs() < f32::EPSILON);
    assert_eq!(
        d.target_folder, None,
        "confiance clampe a 0 < seuil -> INBOX"
    );
}

#[test]
fn les_champs_du_verdict_sont_repris_dans_la_decision() {
    let mut v = verdict("Newsletters", 0.9);
    v.is_newsletter = true;
    v.summary = "promo du mois".into();
    v.priority = 4;
    let d = decide(&whitelist(), &v, 0.8, "copy", false).unwrap();
    assert_eq!(d.summary, "promo du mois");
    assert!(d.is_newsletter);
    assert_eq!(d.priority, 4);
    assert!((d.confidence - 0.9).abs() < f32::EPSILON);
}
