use email_zero::store::{Store, TriageRecord};
use email_zero::triage::{RoutingRule, TriageAction, TriageDecision};

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
        rule: RoutingRule::MeilleurCandidat,
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

// ---- description_source : gestion manuelle des descriptions (source llm/user) ----

use email_zero::store::DescriptionSource;

#[test]
fn migration_ajoute_description_source_a_une_db_legacy() {
    // DB creee avec l'ancien schema (sans description_source) : l'ouverture ajoute
    // la colonne avec le defaut 'llm', les donnees existantes restent lisibles et
    // la reouverture est idempotente.
    let dir = std::env::temp_dir().join(format!(
        "ez-legacy-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("legacy.db");
    let path_str = path.to_str().unwrap().to_string();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE mailbox (id TEXT PRIMARY KEY);
             CREATE TABLE folder (
               mailbox_id TEXT NOT NULL REFERENCES mailbox(id),
               name TEXT NOT NULL,
               description TEXT,
               examples TEXT,
               \"excluded\" INTEGER NOT NULL DEFAULT 0,
               updated_at TEXT NOT NULL,
               PRIMARY KEY (mailbox_id, name)
             );
             INSERT INTO mailbox (id) VALUES ('orange');
             INSERT INTO folder (mailbox_id, name, description, \"excluded\", updated_at)
               VALUES ('orange', 'Factures', 'factures anciennes', 0, '2026-11-01T00:00:00Z');",
        )
        .unwrap();
    }
    {
        let mut store = Store::open(&path_str).unwrap();
        let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
        assert_eq!(p.description.as_deref(), Some("factures anciennes"));
        assert_eq!(
            p.description_source,
            DescriptionSource::Llm,
            "defaut 'llm' sur l'ancienne donnee"
        );
        // la colonne existe bien pour les ecritures suivantes
        store
            .set_user_folder_description("orange", "Factures", "desc manuelle", &[])
            .unwrap();
        let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
        assert_eq!(p.description_source, DescriptionSource::User);
        drop(store);
        // reouverture : la migration ne se rejoue pas sans erreur
        let store = Store::open(&path_str).unwrap();
        let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
        assert_eq!(p.description_source, DescriptionSource::User);
        assert_eq!(p.description.as_deref(), Some("desc manuelle"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn description_utilisateur_persiste_avec_la_source_user_et_les_exemples_fournis() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    store
        .set_user_folder_description(
            "orange",
            "Factures",
            "factures, recus, relances",
            &["Facture octobre".to_string(), "Recu amazon".to_string()],
        )
        .unwrap();
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("factures, recus, relances"));
    assert_eq!(
        p.examples,
        vec!["Facture octobre".to_string(), "Recu amazon".to_string()],
        "exemples fournis avec la description manuelle"
    );
    assert_eq!(p.description_source, DescriptionSource::User);
    assert!(!p.updated_at.is_empty(), "updated_at mis a jour");
}

#[test]
fn description_utilisateur_sans_exemples_purge_les_exemples_llm() {
    // regle retenue : une saisie manuelle sans exemples purge les exemples du
    // profilage LLM — ils ne sont jamais laisses figes en contradiction avec la
    // nouvelle description.
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    store
        .set_folder_profile(
            "orange",
            "Factures",
            Some("factures llm"),
            &["Facture octobre".to_string(), "Recu amazon".to_string()],
        )
        .unwrap();
    store
        .set_user_folder_description("orange", "Factures", "desc manuelle", &[])
        .unwrap();
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("desc manuelle"));
    assert!(
        p.examples.is_empty(),
        "exemples LLM purges a la saisie manuelle"
    );
    assert_eq!(p.description_source, DescriptionSource::User);
}

#[test]
fn le_profilage_llm_n_ecrase_jamais_une_description_utilisateur() {
    // précédence : set_folder_profile (chemin LLM du discovery) laisse intacte une
    // ligne en source 'user' (description, exemples, source, updated_at).
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    store
        .set_user_folder_description(
            "orange",
            "Factures",
            "desc manuelle",
            &["exemple manuel".to_string()],
        )
        .unwrap();
    let avant = store.folder_profile("orange", "Factures").unwrap().unwrap();
    store
        .set_folder_profile(
            "orange",
            "Factures",
            Some("desc llm"),
            &["ex llm".to_string()],
        )
        .unwrap();
    let apres = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(apres.description, avant.description);
    assert_eq!(
        apres.examples,
        vec!["exemple manuel".to_string()],
        "les exemples manuels survivent aussi au profilage LLM"
    );
    assert_eq!(apres.description_source, DescriptionSource::User);
    assert_eq!(
        apres.updated_at, avant.updated_at,
        "ligne non touchee du tout"
    );
}

#[test]
fn le_profilage_llm_ecrit_avec_la_source_llm_sur_un_dossier_llm() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    store
        .set_folder_profile("orange", "Factures", Some("factures llm"), &[])
        .unwrap();
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("factures llm"));
    assert_eq!(p.description_source, DescriptionSource::Llm);
}

#[test]
fn reset_remet_la_source_llm_et_le_profilage_suivant_reecrit() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    store
        .set_user_folder_description("orange", "Factures", "desc manuelle", &[])
        .unwrap();
    store
        .reset_folder_description_source("orange", "Factures")
        .unwrap();
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description_source, DescriptionSource::Llm);
    assert_eq!(
        p.description.as_deref(),
        Some("desc manuelle"),
        "la description courante reste en place jusqu'au prochain profilage"
    );
    // et le profilage LLM peut a nouveau ecrire (description ET exemples)
    store
        .set_folder_profile(
            "orange",
            "Factures",
            Some("desc llm 2"),
            &["ex llm 2".to_string()],
        )
        .unwrap();
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("desc llm 2"));
    assert_eq!(p.examples, vec!["ex llm 2".to_string()]);
    assert_eq!(p.description_source, DescriptionSource::Llm);
}

#[test]
fn set_user_et_reset_sur_dossier_inconnu_sont_rejetes() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    let err = store
        .set_user_folder_description("orange", "Inconnu", "x", &[])
        .unwrap_err();
    assert!(err.to_string().contains("Inconnu"), "{err}");
    let err = store
        .reset_folder_description_source("orange", "Inconnu")
        .unwrap_err();
    assert!(err.to_string().contains("Inconnu"), "{err}");
}

#[test]
fn folders_liste_les_dossiers_avec_source_exclusion_et_exemples() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders(
            "orange",
            &[
                ("Factures", None, false),
                ("Perso", None, false),
                ("Trash", None, true),
            ],
        )
        .unwrap();
    store
        .set_folder_profile(
            "orange",
            "Factures",
            Some("factures llm"),
            &["Facture octobre".to_string()],
        )
        .unwrap();
    store
        .set_user_folder_description("orange", "Perso", "mails perso manuels", &[])
        .unwrap();
    let all = store.folders("orange").unwrap();
    let noms: Vec<&str> = all.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(noms, vec!["Factures", "Perso", "Trash"], "ordre alpha");
    assert_eq!(all[0].description_source, DescriptionSource::Llm);
    assert!(!all[0].excluded);
    assert_eq!(all[0].examples, vec!["Facture octobre".to_string()]);
    assert_eq!(all[1].description_source, DescriptionSource::User);
    assert_eq!(all[1].description.as_deref(), Some("mails perso manuels"));
    assert!(
        all[1].examples.is_empty(),
        "exemples purges a la saisie manuelle"
    );
    assert!(all[2].excluded, "dossier exclu remonte aussi");
}

#[test]
fn user_described_folders_ne_retourne_que_les_sources_user() {
    let mut store = open_store();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders(
            "orange",
            &[("Factures", None, false), ("Perso", None, false)],
        )
        .unwrap();
    assert!(store.user_described_folders("orange").unwrap().is_empty());
    store
        .set_folder_profile("orange", "Factures", Some("factures llm"), &[])
        .unwrap();
    store
        .set_user_folder_description("orange", "Perso", "perso manuel", &[])
        .unwrap();
    assert_eq!(
        store.user_described_folders("orange").unwrap(),
        vec!["Perso".to_string()]
    );
}
