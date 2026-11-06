use email_zero::config::Config;
use email_zero::folders::{run_folder_command, FolderCli, FolderCommand};
use email_zero::store::{DescriptionSource, Store};

fn test_config() -> Config {
    let toml = r#"
[[mailbox]]
id = "orange"
host = "127.0.0.1"
port = 1143
username = "a@b.c"
password_env = "EZ_TEST_PW"

[[mailbox]]
id = "proton"
host = "127.0.0.1"
port = 1143
username = "p@b.c"
password_env = "EZ_TEST_PW"

[llm]
base_url = "https://llm.example.com/v1"
api_key_env = "EZ_TEST_KEY"
model = "m"

[store]
path = ":memory:"
"#;
    Config::parse(toml).unwrap()
}

/// orange : Factures (profil llm avec exemples) + Trash (exclu) ; proton : Factures
fn setup_store() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .sync_mailboxes(&["orange".to_string(), "proton".to_string()])
        .unwrap();
    store
        .upsert_folders(
            "orange",
            &[("Factures", None, false), ("Trash", None, true)],
        )
        .unwrap();
    store
        .upsert_folders("proton", &[("Factures", None, false)])
        .unwrap();
    store
        .set_folder_profile(
            "orange",
            "Factures",
            Some("factures et recus (profil llm)"),
            &["Facture octobre".to_string(), "Recu amazon".to_string()],
        )
        .unwrap();
    store
}

fn run_cmd(
    cfg: &Config,
    store: &mut Store,
    cmd: &FolderCommand,
    mailbox: Option<&str>,
    stdin: &str,
) -> (email_zero::error::Result<()>, String) {
    let mut out: Vec<u8> = Vec::new();
    let mut stdin_bytes = stdin.as_bytes();
    let res = run_folder_command(cfg, store, cmd, mailbox, &mut stdin_bytes, &mut out);
    (res, String::from_utf8(out).unwrap())
}

fn describe_cmd(
    folder: &str,
    text: Option<String>,
    examples: Option<Vec<String>>,
) -> FolderCommand {
    FolderCommand::Describe {
        folder: folder.into(),
        text,
        examples,
    }
}

#[test]
fn list_folders_affiche_source_whitelist_description_et_exemples_par_boite() {
    let cfg = test_config();
    let mut store = setup_store();
    let (res, out) = run_cmd(&cfg, &mut store, &FolderCommand::List, None, "");
    res.unwrap();
    assert!(out.contains("== orange =="), "out: {out}");
    assert!(out.contains("== proton =="), "out: {out}");
    assert!(
        out.contains("Factures [source=llm] [whitelist]"),
        "source llm + whitelist affiches: {out}"
    );
    assert!(
        out.contains("Trash [source=llm] [hors whitelist]"),
        "dossier exclu affiche: {out}"
    );
    assert!(
        out.contains("description : factures et recus (profil llm)"),
        "out: {out}"
    );
    assert!(
        out.contains("exemples (2) : \"Facture octobre\", \"Recu amazon\""),
        "out: {out}"
    );
}

#[test]
fn list_folders_boite_sans_dossier_dit_que_la_decouverte_n_a_pas_tourne() {
    let cfg = test_config();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    let (res, out) = run_cmd(&cfg, &mut store, &FolderCommand::List, Some("orange"), "");
    res.unwrap();
    assert!(out.contains("aucun dossier"), "out: {out}");
}

#[test]
fn describe_folder_affiche_l_etat_actuel_puis_ecrit_la_nouvelle_description() {
    let cfg = test_config();
    let mut store = setup_store();
    let cmd = describe_cmd(
        "Factures",
        Some("factures UNIQUEMENT, jamais de marketing".into()),
        None,
    );
    let (res, out) = run_cmd(&cfg, &mut store, &cmd, Some("orange"), "");
    res.unwrap();
    assert!(
        out.contains("description actuelle [source=llm] : factures et recus (profil llm)"),
        "etat actuel affiche: {out}"
    );
    assert!(
        out.contains("exemples (2)"),
        "exemples actuels affiches: {out}"
    );
    assert!(
        out.contains(
            "nouvelle description [source=user] : factures UNIQUEMENT, jamais de marketing"
        ),
        "confirmation: {out}"
    );
    assert!(
        out.contains("exemples : aucun"),
        "la confirmation reflete la purge des exemples: {out}"
    );
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(
        p.description.as_deref(),
        Some("factures UNIQUEMENT, jamais de marketing")
    );
    assert_eq!(p.description_source, DescriptionSource::User);
    assert!(
        p.examples.is_empty(),
        "sans --describe-examples, les exemples LLM sont purges"
    );
    // l'autre boite n'est pas touchee quand --mailbox est precise
    let p = store.folder_profile("proton", "Factures").unwrap().unwrap();
    assert_eq!(p.description, None);
    assert_eq!(p.description_source, DescriptionSource::Llm);
}

#[test]
fn describe_folder_avec_examples_fournis_remplace_les_exemples() {
    let cfg = test_config();
    let mut store = setup_store();
    let cmd = describe_cmd(
        "Factures",
        Some("factures uniquement".into()),
        Some(vec!["Facture EDF".into(), "Recu amazon".into()]),
    );
    let (res, out) = run_cmd(&cfg, &mut store, &cmd, Some("orange"), "");
    res.unwrap();
    assert!(
        out.contains("exemples (2) : \"Facture EDF\", \"Recu amazon\""),
        "la confirmation reflete les exemples fournis: {out}"
    );
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(
        p.examples,
        vec!["Facture EDF".to_string(), "Recu amazon".to_string()]
    );
    assert_eq!(p.description_source, DescriptionSource::User);
}

#[test]
fn describe_folder_lit_le_texte_multiligne_sur_stdin_avec_un_indice_avant_la_lecture() {
    let cfg = test_config();
    let mut store = setup_store();
    let cmd = describe_cmd("Factures", None, None);
    let (res, out) = run_cmd(&cfg, &mut store, &cmd, Some("orange"), "ligne 1\nligne 2\n");
    res.unwrap();
    assert!(
        out.contains("stdin"),
        "un indice annonce la lecture stdin avant le EOF: {out}"
    );
    assert!(
        out.contains("description actuelle [source=llm]"),
        "l'etat actuel est affiche avant la lecture: {out}"
    );
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("ligne 1\nligne 2"));
    assert_eq!(p.description_source, DescriptionSource::User);
}

#[test]
fn describe_folder_sans_mailbox_s_applique_a_toutes_les_boites() {
    let cfg = test_config();
    let mut store = setup_store();
    let cmd = describe_cmd("Factures", Some("desc commune".into()), None);
    let (res, out) = run_cmd(&cfg, &mut store, &cmd, None, "");
    res.unwrap();
    assert!(out.contains("== orange / Factures =="), "out: {out}");
    assert!(out.contains("== proton / Factures =="), "out: {out}");
    for mb in ["orange", "proton"] {
        let p = store.folder_profile(mb, "Factures").unwrap().unwrap();
        assert_eq!(p.description.as_deref(), Some("desc commune"));
        assert_eq!(p.description_source, DescriptionSource::User);
    }
}

#[test]
fn describe_multi_boites_pre_verifie_l_existence_sur_toutes_les_cibles_avant_d_ecrire() {
    // si le dossier manque dans UNE cible, aucune cible n'est ecrite (pas d'effet
    // partiel multi-boites).
    let cfg = test_config();
    let mut store = Store::open_in_memory().unwrap();
    store
        .sync_mailboxes(&["orange".to_string(), "proton".to_string()])
        .unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    store
        .set_folder_profile("orange", "Factures", Some("profil llm orange"), &[])
        .unwrap();
    // proton n'a PAS le dossier Factures
    let cmd = describe_cmd("Factures", Some("desc commune".into()), None);
    let (res, _out) = run_cmd(&cfg, &mut store, &cmd, None, "");
    let err = res.unwrap_err();
    assert!(err.to_string().contains("proton"), "{err}");
    assert!(err.to_string().contains("Factures"), "{err}");
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(
        p.description.as_deref(),
        Some("profil llm orange"),
        "aucune ecriture partielle: orange inchangé"
    );
    assert_eq!(p.description_source, DescriptionSource::Llm);
}

#[test]
fn describe_folder_sur_dossier_inconnu_est_rejete_sans_rien_ecrire() {
    let cfg = test_config();
    let mut store = setup_store();
    let cmd = describe_cmd("Inconnu", Some("x".into()), None);
    let (res, _out) = run_cmd(&cfg, &mut store, &cmd, Some("orange"), "");
    let err = res.unwrap_err();
    assert!(err.to_string().contains("Inconnu"), "{err}");
    assert!(store.folder_profile("orange", "Inconnu").unwrap().is_none());
}

#[test]
fn describe_folder_avec_texte_vide_est_rejete_avant_toute_ecriture() {
    let cfg = test_config();
    let mut store = setup_store();
    let cmd = describe_cmd("Factures", Some("   \n  ".into()), None);
    // sans --mailbox : la validation a lieu avant l'ecriture sur AUCUNE boite
    let (res, _out) = run_cmd(&cfg, &mut store, &cmd, None, "");
    assert!(res.is_err(), "description vide refusee");
    for mb in ["orange", "proton"] {
        let p = store.folder_profile(mb, "Factures").unwrap().unwrap();
        assert_eq!(
            p.description_source,
            DescriptionSource::Llm,
            "rien n'a ete ecrit sur {mb}"
        );
    }
}

#[test]
fn describe_folder_avec_description_trop_longue_est_rejetee_sans_ecrire() {
    let cfg = test_config();
    let mut store = setup_store();
    let longue = "a".repeat(2001);
    let cmd = describe_cmd("Factures", Some(longue), None);
    let (res, _out) = run_cmd(&cfg, &mut store, &cmd, Some("orange"), "");
    let err = res.unwrap_err();
    assert!(
        err.to_string().contains("2000"),
        "borne de longueur mentionnee: {err}"
    );
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(
        p.description_source,
        DescriptionSource::Llm,
        "rien n'a ete ecrit"
    );
    // 2000 caracteres exactement : accepte
    let cmd = describe_cmd("Factures", Some("a".repeat(2000)), None);
    let (res, _out) = run_cmd(&cfg, &mut store, &cmd, Some("orange"), "");
    res.unwrap();
}

#[test]
fn reset_folder_description_remet_la_source_llm_et_le_profilage_peut_reprendre() {
    let cfg = test_config();
    let mut store = setup_store();
    store
        .set_user_folder_description("orange", "Factures", "desc manuelle", &[])
        .unwrap();
    let cmd = FolderCommand::Reset {
        folder: "Factures".into(),
    };
    let (res, out) = run_cmd(&cfg, &mut store, &cmd, Some("orange"), "");
    res.unwrap();
    assert!(out.contains("'user' -> 'llm'"), "out: {out}");
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description_source, DescriptionSource::Llm);
    assert_eq!(p.description.as_deref(), Some("desc manuelle"));
    // et le profilage LLM peut a nouveau ecrire sur ce dossier
    store
        .set_folder_profile("orange", "Factures", Some("desc llm"), &[])
        .unwrap();
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("desc llm"));
}

#[test]
fn reset_sur_dossier_inconnu_est_rejete() {
    let cfg = test_config();
    let mut store = setup_store();
    let cmd = FolderCommand::Reset {
        folder: "Inconnu".into(),
    };
    let (res, _out) = run_cmd(&cfg, &mut store, &cmd, Some("orange"), "");
    assert!(res.unwrap_err().to_string().contains("Inconnu"));
}

#[test]
fn mailbox_inconnue_de_la_config_est_rejetee() {
    let cfg = test_config();
    let mut store = setup_store();
    let (res, _out) = run_cmd(&cfg, &mut store, &FolderCommand::List, Some("nope"), "");
    let err = res.unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
    assert!(
        err.to_string().contains("orange"),
        "les boites connues sont listees: {err}"
    );
}

#[test]
fn validation_deux_commandes_dossier_en_meme_temps_est_rejetee() {
    let cli = FolderCli {
        list_folders: true,
        describe_folder: Some("Factures".into()),
        ..Default::default()
    };
    let err = cli.command().unwrap_err();
    assert!(err.to_string().contains("une seule commande"), "{err}");
}

#[test]
fn validation_describe_text_sans_describe_folder_est_rejetee() {
    let cli = FolderCli {
        describe_text: Some("texte".into()),
        ..Default::default()
    };
    let err = cli.command().unwrap_err();
    assert!(err.to_string().contains("--describe-folder"), "{err}");
}

#[test]
fn validation_describe_examples_sans_describe_folder_est_rejetee() {
    let cli = FolderCli {
        describe_examples: Some("EX1;EX2".into()),
        ..Default::default()
    };
    let err = cli.command().unwrap_err();
    assert!(err.to_string().contains("--describe-folder"), "{err}");
}

#[test]
fn validation_mailbox_sans_commande_dossier_est_rejetee() {
    let cli = FolderCli {
        mailbox: Some("orange".into()),
        ..Default::default()
    };
    let err = cli.command().unwrap_err();
    assert!(err.to_string().contains("--mailbox"), "{err}");
}

#[test]
fn cli_vide_ne_demande_aucune_commande() {
    let cli = FolderCli::default();
    assert!(cli.command().unwrap().is_none());
}

#[test]
fn describe_examples_est_parse_en_liste_separee_par_point_virgule() {
    let cli = FolderCli {
        describe_folder: Some("Factures".into()),
        describe_examples: Some(" EX1 ; ; EX2 ;EX3 ".into()),
        ..Default::default()
    };
    match cli.command().unwrap().unwrap() {
        FolderCommand::Describe { examples, .. } => {
            assert_eq!(
                examples,
                Some(vec![
                    "EX1".to_string(),
                    "EX2".to_string(),
                    "EX3".to_string()
                ]),
                "trim + elements vides ignores"
            );
        }
        other => panic!("Describe attendu, recu {other:?}"),
    }
    // sans --describe-examples : None = purge des exemples existants
    let cli = FolderCli {
        describe_folder: Some("Factures".into()),
        ..Default::default()
    };
    match cli.command().unwrap().unwrap() {
        FolderCommand::Describe { examples, .. } => assert_eq!(examples, None),
        other => panic!("Describe attendu, recu {other:?}"),
    }
}

#[test]
fn describe_examples_trop_nombreux_sont_rejetes() {
    let cli = FolderCli {
        describe_folder: Some("Factures".into()),
        describe_examples: Some(
            (1..=11)
                .map(|i| format!("EX{i}"))
                .collect::<Vec<_>>()
                .join(";"),
        ),
        ..Default::default()
    };
    assert!(cli.command().is_err(), "plus de 10 exemples refuses");
}
