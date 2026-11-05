use email_zero::store::{Store, TriageRecord};
use email_zero::triage::{TriageAction, TriageDecision};

fn open_store() -> Store {
    Store::open_in_memory().expect("store in-memory")
}

#[test]
fn reouverture_d_une_db_fichier_est_idempotente() {
    let dir = std::env::temp_dir().join(format!("email-zero-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.db");
    let path_str = path.to_str().unwrap().to_string();

    {
        let mut store = Store::open(&path_str).unwrap();
        store.sync_mailboxes(&["orange".to_string()]).unwrap();
        store.mark_processed("orange", "INBOX", 42, 7).unwrap();
    }
    {
        // deuxieme ouverture : les CREATE TABLE IF NOT EXISTS repassent sans erreur
        // et les donnees persistent
        let store = Store::open(&path_str).unwrap();
        assert_eq!(store.mailbox_ids().unwrap(), vec!["orange".to_string()]);
        assert!(store.is_processed("orange", "INBOX", 42, 7).unwrap());
    }
    let _ = std::fs::remove_dir_all(&dir);
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
fn upsert_folders_ne_efface_pas_la_description_existante() {
    // revue PR #13 : la description n'appartient qu'a set_folder_profile ;
    // un re-run de decouverte (upsert avec None) ne doit pas l'ecraser, meme
    // si le run echoue ensuite mi-parcours.
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders("orange", &[("Factures", Some("factures et recus"), false)])
        .unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("factures et recus"));
    // le flag excluded, lui, est bien mis a jour par l'upsert
    store
        .upsert_folders("orange", &[("Factures", None, true)])
        .unwrap();
    assert!(!store
        .whitelist("orange")
        .unwrap()
        .contains(&"Factures".to_string()));
}

#[test]
fn last_discovery_run_ignore_les_runs_error_et_running() {
    // revue PR #13 : seuls les runs termines ok/fallback font tourner
    // l'horloge d'intervalle (voir discovery_due).
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store.start_discovery_run("orange").unwrap();
    store.finish_discovery_run("orange", "ok", Some(3)).unwrap();
    store.start_discovery_run("orange").unwrap();
    store.finish_discovery_run("orange", "error", None).unwrap();
    store.start_discovery_run("orange").unwrap(); // running orphelin (crash)
    let last = store.last_discovery_run("orange").unwrap().unwrap();
    assert_eq!(last.status, "ok", "error/running ignores");
    assert_eq!(last.folders_profiled, Some(3));
    // historique complet conserve par discovery_runs
    assert_eq!(store.discovery_runs("orange").unwrap().len(), 3);
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
            Some("factures, recus, impots"),
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

#[test]
fn ouverture_cree_le_repertoire_parent_du_fichier_sqlite() {
    let dir = std::env::temp_dir().join(format!(
        "ez-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("sous-dossier/state.sqlite");
    let path_str = path.to_str().unwrap().to_string();
    let store = Store::open(&path_str).expect("ouverture avec creation du parent");
    assert!(path.exists());
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
