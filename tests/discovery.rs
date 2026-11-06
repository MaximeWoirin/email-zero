mod common;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use email_zero::clients::llm::OpenAiLlmClient;
use email_zero::config::Config;
use email_zero::discovery::{
    is_excluded, run_discovery, DiscoveryParams, DiscoveryStats, PROFILING_SCHEMA,
};
use email_zero::error::{Error, Result};
use email_zero::ports::{Folder, JsonChatRequest, LlmClient, MailClient, MailHeader, ParsedMail};
use email_zero::runner::{poll_mailbox, real_connect, PollOptions};
use email_zero::store::Store;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct FakeMail {
    folders: RefCell<Vec<Folder>>,
    fail_list: bool,
    mails: HashMap<(String, u32), ParsedMail>,
    search_all_calls: RefCell<usize>,
}

impl FakeMail {
    fn new(folders: Vec<Folder>) -> Self {
        Self {
            folders: RefCell::new(folders),
            fail_list: false,
            mails: HashMap::new(),
            search_all_calls: RefCell::new(0),
        }
    }

    fn add_folder(&self, name: &str, special_use: Option<&str>) {
        self.folders.borrow_mut().push(Folder {
            name: name.into(),
            special_use: special_use.map(|s| s.to_string()),
        });
    }

    fn add_mail(&mut self, folder: &str, uid: u32, from: &str, subject: &str) {
        self.mails.insert(
            (folder.to_string(), uid),
            ParsedMail {
                header: MailHeader {
                    subject: subject.into(),
                    from: from.into(),
                    date: "2026-11-01T10:00:00Z".into(),
                },
                body_text: format!("corps de {subject}"),
            },
        );
    }
}

impl MailClient for FakeMail {
    async fn list_folders(&mut self) -> Result<Vec<Folder>> {
        if self.fail_list {
            return Err(Error::Imap("LIST en panne (test)".into()));
        }
        Ok(self.folders.borrow().clone())
    }
    async fn uidvalidity(&mut self, _folder: &str) -> Result<u32> {
        Ok(1)
    }
    async fn search_unseen(&mut self, folder: &str) -> Result<Vec<u32>> {
        self.search_all(folder).await
    }
    async fn search_all(&mut self, folder: &str) -> Result<Vec<u32>> {
        *self.search_all_calls.borrow_mut() += 1;
        let mut uids: Vec<u32> = self
            .mails
            .keys()
            .filter(|(f, _)| f == folder)
            .map(|(_, u)| *u)
            .collect();
        uids.sort_unstable();
        Ok(uids)
    }
    async fn fetch_headers(&mut self, folder: &str, uid: u32) -> Result<MailHeader> {
        Ok(self
            .mails
            .get(&(folder.to_string(), uid))
            .ok_or_else(|| Error::Store(format!("mail absent {folder}/{uid}")))?
            .header
            .clone())
    }
    async fn fetch_body(&mut self, folder: &str, uid: u32) -> Result<ParsedMail> {
        Ok(self
            .mails
            .get(&(folder.to_string(), uid))
            .ok_or_else(|| Error::Store(format!("mail absent {folder}/{uid}")))?
            .clone())
    }
    async fn copy(&mut self, _folder: &str, _uid: u32, _target: &str) -> Result<()> {
        Ok(())
    }
    async fn r#move(&mut self, _folder: &str, _uid: u32, _target: &str) -> Result<()> {
        Ok(())
    }
}

struct FakeLlm {
    responses: RefCell<VecDeque<Result<serde_json::Value>>>,
    requests: RefCell<Vec<JsonChatRequest>>,
    schema_calls: RefCell<usize>,
}

impl FakeLlm {
    fn new(responses: Vec<Result<serde_json::Value>>) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            requests: RefCell::new(Vec::new()),
            schema_calls: RefCell::new(0),
        }
    }

    fn ok(responses: Vec<serde_json::Value>) -> Self {
        Self::new(responses.into_iter().map(Ok).collect())
    }

    fn down() -> Self {
        Self::new(vec![Err(Error::Llm("LLM indisponible (test)".into()))])
    }

    fn last_user_prompt(&self) -> String {
        self.requests
            .borrow()
            .last()
            .expect("au moins une requete")
            .messages
            .iter()
            .find(|m| m.role == "user")
            .expect("message user present")
            .content
            .clone()
    }
}

impl LlmClient for FakeLlm {
    async fn chat_json<T: DeserializeOwned>(&self, req: JsonChatRequest) -> Result<T> {
        self.requests.borrow_mut().push(req);
        let v = self
            .responses
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| Error::Llm("fake llm: plus de reponse scriptee".into()))?;
        Ok(serde_json::from_value(v?)?)
    }
    async fn chat_json_schema<T: DeserializeOwned>(
        &self,
        req: JsonChatRequest,
        _schema: &str,
    ) -> Result<T> {
        *self.schema_calls.borrow_mut() += 1;
        self.chat_json(req).await
    }
}

fn params() -> DiscoveryParams {
    DiscoveryParams {
        sample_per_folder: 5,
        exclude_folders: vec![
            "Sent".into(),
            "Drafts".into(),
            "Trash".into(),
            "Junk".into(),
        ],
        system_prompt: "PROMPT PROFILAGE DE TEST".into(),
        model: "modele-test".into(),
        temperature: 0.2,
        top_p: 1.0,
        max_tokens: 4096,
    }
}

fn profile(name: &str, description: &str, examples: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "examples": examples,
    })
}

fn profiling_response(profiles: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "folders": profiles })
}

fn setup_store() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
}

#[tokio::test]
async fn special_use_exclus_par_defaut_mais_archive_conserve_et_inbox_hors_whitelist() {
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "INBOX".into(),
            special_use: None,
        },
        Folder {
            name: "Sent".into(),
            special_use: Some("\\Sent".into()),
        },
        Folder {
            name: "Drafts".into(),
            special_use: Some("\\Drafts".into()),
        },
        Folder {
            name: "Trash".into(),
            special_use: Some("\\Trash".into()),
        },
        Folder {
            name: "Junk".into(),
            special_use: Some("\\Junk".into()),
        },
        Folder {
            name: "Archive".into(),
            special_use: Some("\\Archive".into()),
        },
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("INBOX", 1, "a@x.fr", "mail dans inbox");
    mail.add_mail("Archive", 1, "a@x.fr", "mail archive");
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    let llm = FakeLlm::ok(vec![profiling_response(vec![
        profile("Archive", "mails archives", &["mail archive"]),
        profile("Factures", "factures et recus", &["Facture octobre"]),
    ])]);
    let mut store = setup_store();

    let stats = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();

    let whitelist = store.whitelist("orange").unwrap();
    assert_eq!(
        whitelist,
        vec!["Archive".to_string(), "Factures".to_string()],
        "Sent/Drafts/Trash/Junk exclus (special-use), Archive conserve, INBOX hors whitelist (dossier source)"
    );
    assert_eq!(stats.status, "ok");
    let runs = store.discovery_runs("orange").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "ok");
    assert_eq!(runs[0].folders_profiled, Some(2));
    assert!(runs[0].finished_at.is_some());
    // INBOX est exclu : il n'est meme pas échantillonné ni envoyé au LLM
    let prompt = llm.last_user_prompt();
    assert!(!prompt.contains("INBOX"), "prompt: {prompt}");
    assert!(prompt.contains("Factures"));
}

#[tokio::test]
async fn exclude_folders_config_s_ajoute_aux_exclusions_special_use() {
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "Archive".into(),
            special_use: Some("\\Archive".into()),
        },
        Folder {
            name: "Promotions".into(),
            special_use: None,
        },
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("Archive", 1, "a@x.fr", "vieux mail");
    mail.add_mail("Promotions", 1, "shop@x.fr", "-50%");
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    let llm = FakeLlm::ok(vec![profiling_response(vec![profile(
        "Factures",
        "factures",
        &["Facture octobre"],
    )])]);
    let mut store = setup_store();
    let mut p = params();
    p.exclude_folders = vec![
        "Sent".into(),
        "Drafts".into(),
        "Trash".into(),
        "Junk".into(),
        "Archive".into(),
        "Promotions".into(),
    ];

    run_discovery(&mut mail, &llm, &mut store, "orange", &p)
        .await
        .unwrap();

    assert_eq!(store.whitelist("orange").unwrap(), vec!["Factures"]);
}

#[test]
fn is_excluded_match_par_composant_de_chemin_insensible_a_la_casse_et_sous_arbre() {
    // issue #28 : un dossier est exclu si UN composant de son chemin (separateur
    // `/`) correspond, insensible a la casse, a une entree de exclude_folders —
    // un dossier exclu exclut tout son sous-arbre (matching par prefixe)
    let excl: Vec<String> = vec!["Sent".into(), "Trash".into(), "\\Junk".into()];
    let f = |name: &str, special_use: Option<&str>| Folder {
        name: name.into(),
        special_use: special_use.map(str::to_string),
    };
    // nom exact, quelle que soit la casse
    assert!(is_excluded(&f("Trash", None), &excl));
    assert!(is_excluded(&f("TRASH", None), &excl));
    // sous-arbre entier exclu
    assert!(is_excluded(&f("INBOX/Trash", None), &excl));
    assert!(is_excluded(&f("INBOX/TRASH", None), &excl));
    assert!(is_excluded(&f("INBOX/TRASH/Documents", None), &excl));
    assert!(is_excluded(
        &f("INBOX/TRASH/Certificats inscription", None),
        &excl
    ));
    // special-use RFC 6154 : backslash et casse indifferentiels
    assert!(is_excluded(&f("Spam", Some("\\junk")), &excl));
    assert!(is_excluded(
        &f("Courrier indesirable", Some("\\Junk")),
        &excl
    ));
    // dossier source INBOX toujours exclu
    assert!(is_excluded(&f("INBOX", None), &excl));
    // pas de match partiel sur un composant
    assert!(!is_excluded(&f("Trashy", None), &excl));
    assert!(!is_excluded(&f("INBOX/TrashOld", None), &excl));
    assert!(!is_excluded(&f("Factures", None), &excl));
    assert!(!is_excluded(&f("INBOX/Documents", None), &excl));
}

#[tokio::test]
async fn sous_arbre_exclu_par_composant_insensible_a_la_casse_cas_inbox_trash() {
    // smoke du 2026-11-06 soir (issue #28) : la boite contient INBOX/Trash
    // (special-use) ET INBOX/TRASH (majuscules, dossier distinct de "Trash")
    // avec 3 sous-dossiers — "Trash" dans exclude_folders exclut les deux
    // sous-arbres, quelle que soit la casse.
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "INBOX".into(),
            special_use: None,
        },
        Folder {
            name: "INBOX/Trash".into(),
            special_use: Some("\\Trash".into()),
        },
        Folder {
            name: "INBOX/TRASH".into(),
            special_use: None,
        },
        Folder {
            name: "INBOX/TRASH/Certificats inscription".into(),
            special_use: None,
        },
        Folder {
            name: "INBOX/TRASH/Documents".into(),
            special_use: None,
        },
        Folder {
            name: "INBOX/TRASH/Notes".into(),
            special_use: None,
        },
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("INBOX", 1, "a@x.fr", "mail recu");
    mail.add_mail("INBOX/TRASH/Documents", 1, "a@x.fr", "vieux doc");
    mail.add_mail("INBOX/TRASH/Notes", 1, "a@x.fr", "vieille note");
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    let llm = FakeLlm::ok(vec![profiling_response(vec![profile(
        "Factures",
        "factures",
        &["Facture octobre"],
    )])]);
    let mut store = setup_store();

    let stats = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();

    assert_eq!(store.whitelist("orange").unwrap(), vec!["Factures"]);
    assert_eq!(stats.folders_seen, 7);
    assert_eq!(stats.whitelisted, 1);
    // un seul dossier echantillonne (Factures) : INBOX (source) et tout le
    // sous-arbre TRASH sont exclus des le LIST, jamais envoyes au LLM
    assert_eq!(*mail.search_all_calls.borrow(), 1);
    let prompt = llm.last_user_prompt();
    assert!(
        !prompt.contains("TRASH"),
        "les dossiers exclus ne sont pas envoyes au LLM: {prompt}"
    );
    assert!(prompt.contains("Factures"));
}

#[tokio::test]
async fn echantillonnage_les_n_derniers_headers_par_dossier_alimentent_le_prompt() {
    let mut mail = FakeMail::new(vec![Folder {
        name: "Factures".into(),
        special_use: None,
    }]);
    for uid in 1..=25u32 {
        mail.add_mail("Factures", uid, "edf@edf.fr", &format!("sujet-{uid}"));
    }
    let llm = FakeLlm::ok(vec![profiling_response(vec![profile(
        "Factures",
        "factures",
        &["sujet-25"],
    )])]);
    let mut store = setup_store();
    // sample_per_folder = 5 dans params()

    run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();

    let prompt = llm.last_user_prompt();
    for uid in 21..=25u32 {
        assert!(
            prompt.contains(&format!("sujet-{uid}")),
            "le dernier echantillon sujet-{uid} doit etre dans le prompt: {prompt}"
        );
    }
    assert!(
        !prompt.contains("sujet-20"),
        "au-dela de sample_per_folder, les vieux sujets ne sont pas envoyes: {prompt}"
    );
    assert!(
        prompt.contains("edf@edf.fr"),
        "les expediteurs sont dans le prompt: {prompt}"
    );
}

#[tokio::test]
async fn une_seule_requete_llm_pour_tous_les_dossiers_de_la_boite() {
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
        Folder {
            name: "Newsletters".into(),
            special_use: None,
        },
        Folder {
            name: "Perso".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    mail.add_mail("Newsletters", 1, "news@shop.fr", "Promo");
    mail.add_mail("Perso", 1, "maman@x.fr", "Diner samedi");
    let llm = FakeLlm::ok(vec![profiling_response(vec![
        profile("Factures", "factures", &["Facture octobre"]),
        profile("Newsletters", "newsletters", &["Promo"]),
        profile("Perso", "mails perso", &["Diner samedi"]),
    ])]);
    let mut store = setup_store();

    let stats: DiscoveryStats = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();

    assert_eq!(
        *llm.schema_calls.borrow(),
        1,
        "une seule requete LLM (structured output) pour tous les dossiers"
    );
    assert_eq!(stats.status, "ok");
    assert_eq!(stats.profiled, 3);
}

#[tokio::test]
async fn profils_llm_reassocies_par_nom_et_sauvegardes_avec_updated_at() {
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
        Folder {
            name: "Newsletters".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    mail.add_mail("Newsletters", 1, "news@shop.fr", "Promo");
    // reponse volontairement dans le desordre : la reassociation se fait par `name`
    let llm = FakeLlm::ok(vec![profiling_response(vec![
        profile("Newsletters", "newsletters et promotions", &["Promo"]),
        profile(
            "Factures",
            "factures et justificatifs",
            &["Facture octobre", "Recu amazon"],
        ),
    ])]);
    let mut store = setup_store();

    run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();

    let p_factures = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(
        p_factures.description.as_deref(),
        Some("factures et justificatifs")
    );
    assert_eq!(p_factures.examples, vec!["Facture octobre", "Recu amazon"]);
    assert!(!p_factures.updated_at.is_empty(), "updated_at renseigne");
    let p_news = store
        .folder_profile("orange", "Newsletters")
        .unwrap()
        .unwrap();
    assert_eq!(
        p_news.description.as_deref(),
        Some("newsletters et promotions")
    );
}

#[tokio::test]
async fn llm_indisponible_statut_fallback_echantillons_bruts_sans_description() {
    let mut mail = FakeMail::new(vec![Folder {
        name: "Factures".into(),
        special_use: None,
    }]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    mail.add_mail("Factures", 2, "amazon@x.fr", "Recu amazon");
    let llm = FakeLlm::down();
    let mut store = setup_store();

    let stats = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .expect("fallback, pas d'erreur propagee");

    assert_eq!(stats.status, "fallback");
    // la whitelist est quand meme construite (LIST a marche)
    assert_eq!(store.whitelist("orange").unwrap(), vec!["Factures"]);
    // fallback : echantillons bruts (sujets) gardes, SANS description semantique
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description, None, "pas de description en fallback");
    assert_eq!(p.examples, vec!["Facture octobre", "Recu amazon"]);
    let runs = store.discovery_runs("orange").unwrap();
    assert_eq!(runs[0].status, "fallback");
    assert_eq!(runs[0].folders_profiled, None);
}

#[tokio::test]
async fn dossier_vide_description_inconnue_et_hors_prompt_llm() {
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
        Folder {
            name: "Vide".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    let llm = FakeLlm::ok(vec![profiling_response(vec![profile(
        "Factures",
        "factures",
        &["Facture octobre"],
    )])]);
    let mut store = setup_store();

    let stats = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();

    assert_eq!(stats.status, "ok");
    let p = store.folder_profile("orange", "Vide").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("inconnue"));
    assert!(p.examples.is_empty());
    let prompt = llm.last_user_prompt();
    assert!(
        !prompt.contains("Vide"),
        "un dossier vide n'est pas envoye au LLM (il ne l'utiliserait pas): {prompt}"
    );
    // revue PR #13 : folders_profiled compte les profils LLM effectifs (1),
    // pas la taille de la whitelist (2) — le dossier vide n'est pas "profile"
    let runs = store.discovery_runs("orange").unwrap();
    assert_eq!(runs[0].folders_profiled, Some(1));
}

#[tokio::test]
async fn echec_mi_parcours_conserve_les_descriptions_du_run_precedent() {
    // revue PR #13 : run 1 ok (profil en cache), run 2 echoue apres le LIST
    // (reponse LLM rejetee) -> la description du run 1 n'est pas perdue.
    let mut mail = FakeMail::new(vec![Folder {
        name: "Factures".into(),
        special_use: None,
    }]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    let llm = FakeLlm::ok(vec![
        profiling_response(vec![profile(
            "Factures",
            "factures run 1",
            &["Facture octobre"],
        )]),
        profiling_response(vec![profile("Inconnu", "orphelin", &["?"])]),
    ]);
    let mut store = setup_store();

    run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();
    assert_eq!(
        store
            .folder_profile("orange", "Factures")
            .unwrap()
            .unwrap()
            .description
            .as_deref(),
        Some("factures run 1")
    );

    let err = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Llm(_)));
    assert_eq!(
        store
            .discovery_runs("orange")
            .unwrap()
            .last()
            .unwrap()
            .status,
        "error"
    );
    // la description du run 1 a survecu a l'echec du run 2
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("factures run 1"));
}

#[tokio::test]
async fn profil_en_double_nomme_le_dossier_fautif() {
    // revue PR #13 : le message d'erreur du doublon nomme le vrai dossier.
    let mut mail = FakeMail::new(vec![Folder {
        name: "Factures".into(),
        special_use: None,
    }]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    let llm = FakeLlm::ok(vec![profiling_response(vec![
        profile("Factures", "factures", &["Facture octobre"]),
        profile("Factures", "encore", &["?"]),
    ])]);
    let mut store = setup_store();

    let err = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Llm(_)));
    assert!(
        err.to_string().contains("Factures"),
        "le message doit nommer le dossier en double: {err}"
    );
}

#[tokio::test]
async fn sample_per_folder_zero_desactive_l_echantillonnage() {
    // revue PR #13 : convention du projet « 0 = desactive » — 0 = aucun
    // echantillonnage (aucune commande SEARCH/FETCH, aucune requete LLM),
    // les dossiers whitelistes recoivent la description "inconnue".
    let mut mail = FakeMail::new(vec![Folder {
        name: "Factures".into(),
        special_use: None,
    }]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    mail.add_mail("Factures", 2, "amazon@x.fr", "Recu amazon");
    let llm = FakeLlm::ok(vec![]); // aucune reponse scriptee : tout appel LLM echouerait
    let mut store = setup_store();
    let mut p = params();
    p.sample_per_folder = 0;

    let stats = run_discovery(&mut mail, &llm, &mut store, "orange", &p)
        .await
        .unwrap();

    assert_eq!(stats.status, "ok");
    assert_eq!(*llm.schema_calls.borrow(), 0, "aucune requete LLM");
    assert_eq!(
        *mail.search_all_calls.borrow(),
        0,
        "aucune commande SEARCH ALL non plus"
    );
    assert_eq!(store.whitelist("orange").unwrap(), vec!["Factures"]);
    let prof = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(prof.description.as_deref(), Some("inconnue"));
    assert!(prof.examples.is_empty());
}

#[tokio::test]
async fn dossier_cree_entre_deux_runs_hors_whitelist_jusqu_au_run_suivant() {
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "INBOX".into(),
            special_use: None,
        },
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    let llm = FakeLlm::ok(vec![
        profiling_response(vec![profile("Factures", "factures", &["Facture octobre"])]),
        profiling_response(vec![
            profile("Factures", "factures", &["Facture octobre"]),
            profile("Nouveau", "dossier apparu entre deux runs", &["hello"]),
        ]),
    ]);
    let mut store = setup_store();

    run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();
    assert_eq!(store.whitelist("orange").unwrap(), vec!["Factures"]);

    // un dossier apparait sur le serveur entre deux decouvertes
    mail.add_folder("Nouveau", None);
    mail.add_mail("Nouveau", 1, "hello@x.fr", "hello");
    // la whitelist vient du store : le nouveau dossier reste invisible
    assert_eq!(
        store.whitelist("orange").unwrap(),
        vec!["Factures"],
        "pas de re-LIST entre deux runs : le dossier ajoute reste hors whitelist"
    );

    run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();
    assert_eq!(
        store.whitelist("orange").unwrap(),
        vec!["Factures", "Nouveau"]
    );
}

#[tokio::test]
async fn erreur_imap_list_enregistre_error_et_propage() {
    let mut mail = FakeMail::new(vec![]);
    mail.fail_list = true;
    let llm = FakeLlm::ok(vec![]);
    let mut store = setup_store();

    let err = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Imap(_)), "err: {err}");
    let runs = store.discovery_runs("orange").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "error");
    assert!(runs[0].finished_at.is_some());
    assert_eq!(runs[0].folders_profiled, None);
}

#[tokio::test]
async fn profil_orphelin_ou_manquant_dans_la_reponse_llm_est_rejete() {
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
        Folder {
            name: "Perso".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    mail.add_mail("Perso", 1, "maman@x.fr", "Diner");
    // profil pour un dossier inconnu + pas de profil pour Perso
    let llm = FakeLlm::ok(vec![profiling_response(vec![
        profile("Factures", "factures", &["Facture octobre"]),
        profile("Inconnu", "n'existe pas", &["?"]),
    ])]);
    let mut store = setup_store();

    let err = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Llm(_)), "err: {err}");
    let runs = store.discovery_runs("orange").unwrap();
    assert_eq!(runs[0].status, "error");
}

#[test]
fn profiling_schema_est_un_json_schema_strict_avec_name_description_examples() {
    let v: serde_json::Value = serde_json::from_str(PROFILING_SCHEMA).unwrap();
    assert_eq!(v["type"], "json_schema");
    let schema = &v["json_schema"]["schema"];
    assert_eq!(
        schema["properties"]["folders"]["items"]["required"],
        serde_json::json!(["name", "description", "examples"])
    );
}

const E2E_MAIL: &str = "From: Alice <alice@example.com>\r\nTo: bob@example.com\r\nSubject: Facture octobre\r\nDate: Wed, 01 Nov 2026 10:00:00 +0100\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nVotre facture EDF.\r\n";

/// Mini-serveur IMAP loopback pour le test e2e : LOGIN, SELECT (UIDVALIDITY 42),
/// LIST (INBOX, Sent, Factures), UID SEARCH (UNSEEN -> 1, ALL -> 1),
/// UID FETCH (mail complet, PEEK ou non), UID COPY (logge), LOGOUT.
async fn spawn_e2e_imap() -> (u16, Arc<Mutex<Vec<(u32, String)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let copies = Arc::new(Mutex::new(Vec::new()));
    let copies_out = copies.clone();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        sock.write_all(b"* OK fake imap pret\r\n").await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            buf.clear();
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.ends_with(b"\r\n") {
                    break;
                }
            }
            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
            let mut parts = line.splitn(2, ' ');
            let tag = parts.next().unwrap_or("").to_string();
            let cmd = parts.next().unwrap_or("").to_string();
            let (verb, rest) = match cmd.split_once(' ') {
                Some((v, r)) => (v.to_ascii_uppercase(), r.to_string()),
                None => (cmd.to_ascii_uppercase(), String::new()),
            };
            let reply = match verb.as_str() {
                "LOGIN" => format!("{tag} OK LOGIN ok\r\n"),
                "SELECT" | "EXAMINE" => format!(
                    "* 1 EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen)\r\n* OK [UIDVALIDITY 42] UIDs\r\n* OK [UIDNEXT 2] next\r\n{tag} OK SELECT ok\r\n"
                ),
                "LIST" => format!(
                    "* LIST (\\HasNoChildren) \"/\" INBOX\r\n* LIST (\\HasNoChildren \\Sent) \"/\" Sent\r\n* LIST (\\HasNoChildren) \"/\" Factures\r\n{tag} OK LIST ok\r\n"
                ),
                "UID" => {
                    let (sub, args) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
                    match sub {
                        "SEARCH" => {
                            let _ = args;
                            format!("* SEARCH 1\r\n{tag} OK SEARCH ok\r\n")
                        }
                        "FETCH" => {
                            let (uid, query) = common::split_fetch_args(args);
                            common::fetch_reply(&tag, uid, query, E2E_MAIL)
                        }
                        "COPY" => {
                            let mut it = args.splitn(2, ' ');
                            let uid: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
                            let target = it.next().unwrap_or("").trim_matches('"').to_string();
                            copies_out.lock().unwrap().push((uid, target));
                            format!("{tag} OK COPY ok\r\n")
                        }
                        _ => format!("{tag} BAD\r\n"),
                    }
                }
                "LOGOUT" => {
                    let _ = sock
                        .write_all(format!("* BYE\r\n{tag} OK LOGOUT ok\r\n").as_bytes())
                        .await;
                    let _ = sock.shutdown().await;
                    return;
                }
                _ => format!("{tag} OK\r\n"),
            };
            if sock.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    (port, copies)
}

/// Mini-serveur HTTP loopback : sert une file de reponses (une par connexion,
/// dans l'ordre) au format OpenAI chat/completions.
async fn spawn_e2e_llm_queue(
    responses: Vec<String>,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_out = calls.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => return,
            };
            let queue = queue.clone();
            let calls = calls_out.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let head_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + content_length {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let content = queue.lock().unwrap().pop_front().unwrap_or_default();
                calls.fetch_add(1, Ordering::SeqCst);
                let c = serde_json::to_string(&content).unwrap();
                let payload = format!(r#"{{"choices":[{{"message":{{"content":{c}}}}}]}}"#);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                sock.write_all(response.as_bytes()).await.unwrap();
                sock.shutdown().await.ok();
            });
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), calls)
}

/// Integration loopback (jalon 2c) : `poll_mailbox` avec `discover=true` contre
/// de vrais clients (IMAP + LLM) sur mini-serveurs loopback — la decouverte
/// remplit la whitelist + les profils via UNE requete LLM, puis le poll trie
/// INBOX via une seconde requete LLM.
#[tokio::test]
async fn poll_mailbox_avec_discovery_remplit_whitelist_profils_puis_trie() {
    let (imap_port, copies) = spawn_e2e_imap().await;
    let profiling = r#"{"folders":[{"name":"Factures","description":"factures et recus","examples":["Facture octobre"]}]}"#;
    let batch = r#"{"verdicts":[{"uid":1,"priority":1,"summary":"Facture EDF octobre","is_newsletter":false,"candidates":[{"category":"Factures","confidence":0.97}]}]}"#;
    let (llm_base, llm_calls) =
        spawn_e2e_llm_queue(vec![profiling.to_string(), batch.to_string()]).await;

    let dir = std::env::temp_dir().join(format!(
        "ez-e2e-2c-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let triage_prompt = dir.join("triage.md");
    std::fs::write(&triage_prompt, "Tu es l'assistant de tri Email Zero.").unwrap();
    let discovery_prompt = dir.join("discovery.md");
    std::fs::write(&discovery_prompt, "Tu profiles les dossiers.").unwrap();

    let toml = format!(
        r#"
[[mailbox]]
id = "test"
host = "127.0.0.1"
port = {imap_port}
username = "bob@example.com"
password_env = "EZ_TEST_PW"
tls = false
poll_interval_secs = 60

[llm]
base_url = "{llm_base}"
api_key_env = "EZ_TEST_KEY"
model = "modele-test"
batch_size = 15

[llm.retry]
max_attempts = 1
backoff_base_secs = 0

[imap.retry]
max_attempts = 1
backoff_base_secs = 0

[triage]
action = "copy"
confidence_threshold = 0.8
prompt_path = {:?}

[discovery]
sample_per_folder = 20
prompt_path = {:?}

[store]
path = ":memory:"
"#,
        triage_prompt.to_str().unwrap(),
        discovery_prompt.to_str().unwrap()
    );
    let cfg = Config::parse(&toml).unwrap();

    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["test".to_string()]).unwrap();
    let llm = OpenAiLlmClient::new(&cfg.llm, &|name| {
        assert_eq!(name, "EZ_TEST_KEY");
        Some("cle-test".to_string())
    })
    .unwrap();
    let pw = |name: &str| {
        assert_eq!(name, "EZ_TEST_PW");
        Some("pw-test".to_string())
    };

    let stats = poll_mailbox(
        &cfg,
        &llm,
        &mut store,
        &cfg.mailboxes[0],
        PollOptions {
            dry_run: false,
            discover: true,
        },
        &real_connect,
        &pw,
    )
    .await
    .unwrap();

    // decouverte : whitelist depuis LIST (INBOX/Sent exclus), profil LLM en cache
    assert_eq!(store.whitelist("test").unwrap(), vec!["Factures"]);
    let p = store.folder_profile("test", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("factures et recus"));
    assert_eq!(p.examples, vec!["Facture octobre"]);
    let runs = store.discovery_runs("test").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "ok");
    // poll : 1 mail non lu, copie vers Factures
    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.copied, 1);
    assert_eq!(
        copies.lock().unwrap().as_slice(),
        &[(1u32, "Factures".to_string())]
    );
    assert_eq!(
        llm_calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "2 requetes LLM : 1 profilage (tous dossiers) + 1 batch de tri"
    );
    let r = store
        .triage_result("test", "INBOX", 42, 1)
        .unwrap()
        .unwrap();
    assert_eq!(r.target_folder.as_deref(), Some("Factures"));
    assert_eq!(r.action, "copy");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- dossiers a description utilisateur : epargnes par le profilage ----

use email_zero::store::DescriptionSource;

#[tokio::test]
async fn dossier_a_description_utilisateur_est_epargne_par_le_profilage() {
    // run 1 : profilage LLM classique des deux dossiers. Puis l'utilisateur prend
    // la main sur Factures (source 'user'). Run 2 (equivalent --refresh-folders) :
    // Factures n'est ni echantillonne ni envoye au LLM, sa ligne reste intacte,
    // et Perso est re-profile normalement.
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
        Folder {
            name: "Perso".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    mail.add_mail("Perso", 1, "maman@x.fr", "Diner samedi");
    let llm = FakeLlm::ok(vec![
        profiling_response(vec![
            profile("Factures", "factures llm", &["Facture octobre"]),
            profile("Perso", "mails perso", &["Diner samedi"]),
        ]),
        // run 2 : le LLM ne recoit plus Factures, il ne renvoie que Perso
        profiling_response(vec![profile("Perso", "mails perso v2", &["Diner samedi"])]),
    ]);
    let mut store = setup_store();

    run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();
    store
        .set_user_folder_description(
            "orange",
            "Factures",
            "factures et recus UNIQUEMENT, jamais de marketing evenementiel",
        )
        .unwrap();
    let avant = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(avant.description_source, DescriptionSource::User);

    let stats = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();
    assert_eq!(stats.status, "ok");

    let apres = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(
        apres.description, avant.description,
        "la description manuelle n'est pas modifiee par le run 2"
    );
    assert_eq!(apres.examples, avant.examples, "exemples inchanges");
    assert_eq!(apres.description_source, DescriptionSource::User);
    assert_eq!(
        apres.updated_at, avant.updated_at,
        "ligne non touchee du tout"
    );
    // Factures n'a pas ete envoye au LLM au run 2 (uniquement Perso)
    let prompt = llm.last_user_prompt();
    assert!(
        !prompt.contains("Factures"),
        "dossier a description user hors du prompt LLM: {prompt}"
    );
    assert!(prompt.contains("Perso"));
    // et il n'a pas ete re-echantillonne : 2 SEARCH ALL au run 1, 1 seul au run 2
    assert_eq!(*mail.search_all_calls.borrow(), 3);
    // Perso, lui, a bien ete re-profile par le LLM
    let p = store.folder_profile("orange", "Perso").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("mails perso v2"));
    assert_eq!(p.description_source, DescriptionSource::Llm);
}

#[tokio::test]
async fn dossier_a_description_utilisateur_conserve_aussi_en_fallback_llm_indisponible() {
    // meme protection quand le LLM tombe en panne au run 2 (statut fallback) :
    // la description manuelle survit.
    let mut mail = FakeMail::new(vec![
        Folder {
            name: "Factures".into(),
            special_use: None,
        },
        Folder {
            name: "Perso".into(),
            special_use: None,
        },
    ]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    mail.add_mail("Perso", 1, "maman@x.fr", "Diner samedi");
    let llm = FakeLlm::new(vec![
        Ok(profiling_response(vec![
            profile("Factures", "factures llm", &["Facture octobre"]),
            profile("Perso", "mails perso", &["Diner samedi"]),
        ])),
        Err(Error::Llm("LLM indisponible (test)".into())),
    ]);
    let mut store = setup_store();

    run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();
    store
        .set_user_folder_description("orange", "Factures", "desc manuelle figee")
        .unwrap();
    let avant = store.folder_profile("orange", "Factures").unwrap().unwrap();

    let stats = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .expect("fallback, pas d'erreur propagee");
    assert_eq!(stats.status, "fallback");

    let apres = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(apres.description, avant.description);
    assert_eq!(apres.examples, avant.examples);
    assert_eq!(apres.description_source, DescriptionSource::User);
    // Perso a pris le fallback (echantillons bruts, pas de description)
    let p = store.folder_profile("orange", "Perso").unwrap().unwrap();
    assert_eq!(p.description, None);
    assert_eq!(p.examples, vec!["Diner samedi".to_string()]);
}

#[tokio::test]
async fn tous_les_dossiers_en_source_user_plus_de_requete_llm_du_tout() {
    // cas limite : si tous les dossiers whitelistes sont en source 'user', aucun
    // echantillon n'est envoye au LLM (plus de requete de profilage).
    let mut mail = FakeMail::new(vec![Folder {
        name: "Factures".into(),
        special_use: None,
    }]);
    mail.add_mail("Factures", 1, "edf@edf.fr", "Facture octobre");
    let llm = FakeLlm::ok(vec![]); // aucune reponse scriptee : tout appel echouerait
    let mut store = setup_store();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();
    store
        .set_user_folder_description("orange", "Factures", "desc manuelle")
        .unwrap();

    let stats = run_discovery(&mut mail, &llm, &mut store, "orange", &params())
        .await
        .unwrap();

    assert_eq!(stats.status, "ok");
    assert_eq!(*llm.schema_calls.borrow(), 0, "aucune requete LLM");
    assert_eq!(*mail.search_all_calls.borrow(), 0, "aucun echantillonnage");
    let p = store.folder_profile("orange", "Factures").unwrap().unwrap();
    assert_eq!(p.description.as_deref(), Some("desc manuelle"));
    assert_eq!(p.description_source, DescriptionSource::User);
}
