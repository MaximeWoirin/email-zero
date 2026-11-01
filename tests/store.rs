use email_zero::store::{Store, TriageRecord};
use email_zero::triage::{TriageAction, TriageDecision};

fn open_store() -> Store {
    Store::open_in_memory().expect("store in-memory")
}

fn decision(target: Option<&str>, action: TriageAction) -> TriageDecision {
    TriageDecision {
        target_folder: target.map(|s| s.to_string()),
        priority: 2,
        summary: "resume".into(),
        is_newsletter: false,
        confidence: 0.9,
        action,
    }
}

#[test]
fn schema_applique_et_mailbox_synchronise_depuis_config() {
    let mut store = open_store();
    store
        .sync_mailboxes(&["orange".to_string(), "proton".to_string()])
        .unwrap();
    let ids = store.mailbox_ids().unwrap();
    assert_eq!(ids, vec!["orange".to_string(), "proton".to_string()]);
}

#[test]
fn uid_deja_traite_est_deduplique_entre_les_polls() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    assert!(!store.is_processed("orange", "INBOX", 42, 7).unwrap());
    store.mark_processed("orange", "INBOX", 42, 7).unwrap();
    assert!(store.is_processed("orange", "INBOX", 42, 7).unwrap());
    assert!(!store.is_processed("orange", "INBOX", 42, 8).unwrap());
}

#[test]
fn uidvalidity_change_retraite_le_dossier() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store.mark_processed("orange", "INBOX", 42, 7).unwrap();
    assert!(store.is_processed("orange", "INBOX", 42, 7).unwrap());
    // changement d'UIDVALIDITY -> les UIDs de l'ancien monde ne comptent plus
    assert!(!store.is_processed("orange", "INBOX", 43, 7).unwrap());
    let pending = store.pending_uids("orange", "INBOX", 43, &[7, 8]).unwrap();
    assert_eq!(pending, vec![7, 8], "tout le dossier est a retraiter");
    let pending = store.pending_uids("orange", "INBOX", 42, &[7, 8]).unwrap();
    assert_eq!(pending, vec![8], "seuls les UIDs non traites restent");
}

#[test]
fn dossier_exclu_hors_whitelist() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders(
            "orange",
            &[
                ("INBOX", None, false),
                ("Factures", Some("factures et recus"), false),
                ("Sent", None, true),
                ("Trash", None, true),
            ],
        )
        .unwrap();
    let whitelist = store.whitelist("orange").unwrap();
    assert!(whitelist.contains(&"INBOX".to_string()));
    assert!(whitelist.contains(&"Factures".to_string()));
    assert!(
        !whitelist.contains(&"Sent".to_string()),
        "Sent exclu par defaut"
    );
    assert!(!whitelist.contains(&"Trash".to_string()));
}

#[test]
fn triage_result_persiste_la_decision_avec_cle_uidvalidity() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store.mark_processed("orange", "INBOX", 42, 7).unwrap();
    store
        .save_triage_result(
            "orange",
            "INBOX",
            42,
            7,
            &TriageRecord::from_decision(&decision(Some("Factures"), TriageAction::Copy)),
        )
        .unwrap();
    let rec = store
        .triage_result("orange", "INBOX", 42, 7)
        .unwrap()
        .expect("result present");
    assert_eq!(rec.target_folder.as_deref(), Some("Factures"));
    assert_eq!(rec.action, "copy");
    assert_eq!(rec.priority, 2);
    assert!((rec.confidence - 0.9).abs() < f32::EPSILON);
    // meme uid, autre uidvalidity -> pas de resultat (le dossier sera retraite)
    assert!(store
        .triage_result("orange", "INBOX", 99, 7)
        .unwrap()
        .is_none());
}

#[test]
fn triage_result_sans_cible_pour_confiance_sous_seuil() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store.mark_processed("orange", "INBOX", 42, 7).unwrap();
    store
        .save_triage_result(
            "orange",
            "INBOX",
            42,
            7,
            &TriageRecord::from_decision(&decision(None, TriageAction::None)),
        )
        .unwrap();
    let rec = store
        .triage_result("orange", "INBOX", 42, 7)
        .unwrap()
        .expect("result present");
    assert_eq!(rec.target_folder, None);
    assert_eq!(rec.action, "none");
}

#[test]
fn save_triage_result_sans_processed_en_premier_est_rejete() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    let err = store
        .save_triage_result(
            "orange",
            "INBOX",
            42,
            7,
            &TriageRecord::from_decision(&decision(Some("Factures"), TriageAction::Copy)),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("processed"),
        "FK vers processed: {err}"
    );
}

#[test]
fn discovery_run_enregistre_ok_et_fallback() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store.start_discovery_run("orange").unwrap();
    store.finish_discovery_run("orange", "ok", Some(5)).unwrap();
    store.start_discovery_run("orange").unwrap();
    store
        .finish_discovery_run("orange", "fallback", None)
        .unwrap();
    let runs = store.discovery_runs("orange").unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].status, "ok");
    assert_eq!(runs[0].folders_profiled, Some(5));
    assert_eq!(runs[1].status, "fallback");
}

#[test]
fn profile_dossier_mis_en_cache_avec_description_et_exemples() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    store
        .set_folder_profile(
            "orange",
            "Factures",
            "factures, recus, impots",
            &["Re: facture edf".to_string(), "recu amazon".to_string()],
        )
        .unwrap();
    let profile = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(
        profile.description.as_deref(),
        Some("factures, recus, impots")
    );
    assert_eq!(profile.examples.len(), 2);
    assert!(!profile.updated_at.is_empty(), "timestamp ISO 8601 present");
}
