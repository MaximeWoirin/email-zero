use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use email_zero::error::{Error, Result};
use email_zero::pipeline::{run, PipelineConfig, RunStats, BATCH_SCHEMA};
use email_zero::ports::{Folder, JsonChatRequest, LlmClient, MailClient, MailHeader, ParsedMail};
use email_zero::store::{FolderProfile, Store};
use serde::de::DeserializeOwned;

struct FakeMail {
    uidvalidity: HashMap<String, u32>,
    unseen: HashMap<String, Vec<u32>>,
    mails: HashMap<(String, u32), ParsedMail>,
    copies: RefCell<Vec<(u32, String)>>,
    moves: RefCell<Vec<(u32, String)>>,
}

impl FakeMail {
    fn new(uidvalidity: u32) -> Self {
        let mut m = Self {
            uidvalidity: HashMap::new(),
            unseen: HashMap::new(),
            mails: HashMap::new(),
            copies: RefCell::new(Vec::new()),
            moves: RefCell::new(Vec::new()),
        };
        m.uidvalidity.insert("INBOX".into(), uidvalidity);
        m
    }

    fn add_mail(&mut self, uid: u32, from: &str, subject: &str, body: &str) {
        self.mails.insert(
            ("INBOX".into(), uid),
            ParsedMail {
                header: MailHeader {
                    subject: subject.into(),
                    from: from.into(),
                    date: "2026-11-01T10:00:00Z".into(),
                },
                body_text: body.into(),
            },
        );
        self.unseen.entry("INBOX".into()).or_default().push(uid);
    }
}

impl MailClient for FakeMail {
    async fn list_folders(&mut self) -> Result<Vec<Folder>> {
        Ok(vec![])
    }
    async fn uidvalidity(&mut self, folder: &str) -> Result<u32> {
        Ok(*self.uidvalidity.get(folder).unwrap_or(&1))
    }
    async fn search_unseen(&mut self, folder: &str) -> Result<Vec<u32>> {
        Ok(self.unseen.get(folder).cloned().unwrap_or_default())
    }
    async fn search_all(&mut self, folder: &str) -> Result<Vec<u32>> {
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
    async fn copy(&mut self, _folder: &str, uid: u32, target: &str) -> Result<()> {
        self.copies.borrow_mut().push((uid, target.to_string()));
        Ok(())
    }
    async fn r#move(&mut self, _folder: &str, uid: u32, target: &str) -> Result<()> {
        self.moves.borrow_mut().push((uid, target.to_string()));
        Ok(())
    }
}

struct FakeLlm {
    responses: RefCell<VecDeque<serde_json::Value>>,
    requests: RefCell<Vec<JsonChatRequest>>,
    schemas: RefCell<Vec<String>>,
}

impl FakeLlm {
    fn new(responses: Vec<serde_json::Value>) -> Self {
        Self {
            responses: RefCell::new(responses.into()),
            requests: RefCell::new(Vec::new()),
            schemas: RefCell::new(Vec::new()),
        }
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
        Ok(serde_json::from_value(v)?)
    }
    async fn chat_json_schema<T: DeserializeOwned>(
        &self,
        req: JsonChatRequest,
        schema: &str,
    ) -> Result<T> {
        self.schemas.borrow_mut().push(schema.to_string());
        self.chat_json(req).await
    }
}

fn verdict(uid: u64, category: &str, confidence: f32) -> serde_json::Value {
    serde_json::json!({
        "uid": uid,
        "category": category,
        "priority": 2,
        "summary": format!("resume uid {uid}"),
        "is_newsletter": category == "Newsletters",
        "confidence": confidence,
    })
}

fn batch(verdicts: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "verdicts": verdicts })
}

fn setup() -> (Store, PipelineConfig) {
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders(
            "orange",
            &[("Factures", None, false), ("Newsletters", None, false)],
        )
        .unwrap();
    store
        .set_folder_profile(
            "orange",
            "Factures",
            Some("factures et justificatifs"),
            &["Facture EDF octobre".to_string()],
        )
        .unwrap();
    let cfg = PipelineConfig {
        whitelist: vec!["Factures".into(), "Newsletters".into()],
        profiles: vec![
            FolderProfile {
                name: "Factures".into(),
                description: Some("factures et justificatifs".into()),
                examples: vec!["Facture EDF octobre".into()],
                updated_at: "2026-11-01T00:00:00Z".into(),
            },
            FolderProfile {
                name: "Newsletters".into(),
                description: None,
                examples: vec![],
                updated_at: "2026-11-01T00:00:00Z".into(),
            },
        ],
        confidence_threshold: 0.8,
        action: "copy".into(),
        batch_size: 15,
        max_body_chars: 2000,
        examples_in_prompt: 3,
        dry_run: false,
        system_prompt: "PROMPT SYSTEME DE TEST".into(),
        model: "modele-test".into(),
        temperature: 0.2,
        top_p: 1.0,
        max_tokens: 4096,
    };
    (store, cfg)
}

#[tokio::test]
async fn pipeline_complet_fake_imap_fake_llm() {
    let (mut store, cfg) = setup();
    let mut mail = FakeMail::new(7);
    mail.add_mail(
        1,
        "edf@edf.fr",
        "Facture octobre",
        "votre facture est en piece jointe",
    );
    mail.add_mail(2, "news@shop.fr", "Promo de la semaine", "-50% sur tout");
    mail.add_mail(3, "arnaque@x.io", "Vous avez gagne", "cliquez ici");
    let llm = FakeLlm::new(vec![batch(vec![
        verdict(1, "Factures", 0.95),
        verdict(2, "Newsletters", 0.9),
        verdict(3, "DossierInvente", 0.99),
    ])]);

    let stats: RunStats = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    assert_eq!(stats.scanned, 3);
    assert_eq!(stats.pending, 3);
    assert_eq!(stats.decided, 3);
    assert_eq!(stats.copied, 2);
    assert_eq!(stats.left_in_inbox, 1);
    assert_eq!(
        *mail.copies.borrow(),
        vec![(1, "Factures".to_string()), (2, "Newsletters".to_string())]
    );
    assert!(mail.moves.borrow().is_empty());
    for uid in [1u32, 2, 3] {
        assert!(store.is_processed("orange", "INBOX", 7, uid).unwrap());
    }
    let r3 = store
        .triage_result("orange", "INBOX", 7, 3)
        .unwrap()
        .unwrap();
    assert_eq!(r3.target_folder, None);
    assert_eq!(r3.action, "none");
    let r1 = store
        .triage_result("orange", "INBOX", 7, 1)
        .unwrap()
        .unwrap();
    assert_eq!(r1.target_folder.as_deref(), Some("Factures"));
    assert_eq!(r1.action, "copy");
    assert!((r1.confidence - 0.95).abs() < 1e-6);
}

#[tokio::test]
async fn confidence_sous_seuil_reste_dans_inbox_sans_action() {
    let (mut store, cfg) = setup();
    let mut mail = FakeMail::new(1);
    mail.add_mail(5, "a@b.fr", "sujet", "corps");
    let llm = FakeLlm::new(vec![batch(vec![verdict(5, "Factures", 0.5)])]);

    let stats = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    assert_eq!(stats.copied, 0);
    assert_eq!(stats.left_in_inbox, 1);
    assert!(mail.copies.borrow().is_empty());
    let r = store
        .triage_result("orange", "INBOX", 1, 5)
        .unwrap()
        .unwrap();
    assert_eq!(r.target_folder, None);
    assert_eq!(r.action, "none");
}

#[tokio::test]
async fn dry_run_ne_touche_pas_au_serveur_mais_trace_la_decision() {
    let (mut store, mut cfg) = setup();
    cfg.dry_run = true;
    let mut mail = FakeMail::new(1);
    mail.add_mail(1, "edf@edf.fr", "Facture", "corps");
    let llm = FakeLlm::new(vec![batch(vec![verdict(1, "Factures", 0.95)])]);

    let stats = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    assert!(mail.copies.borrow().is_empty());
    assert!(mail.moves.borrow().is_empty());
    assert_eq!(stats.copied, 0);
    let r = store
        .triage_result("orange", "INBOX", 1, 1)
        .unwrap()
        .unwrap();
    assert_eq!(r.target_folder.as_deref(), Some("Factures"));
    assert_eq!(r.action, "dry_run");
    assert!(store.is_processed("orange", "INBOX", 1, 1).unwrap());
}

#[tokio::test]
async fn action_move_appelle_move_pas_copy() {
    let (mut store, mut cfg) = setup();
    cfg.action = "move".into();
    let mut mail = FakeMail::new(1);
    mail.add_mail(1, "edf@edf.fr", "Facture", "corps");
    let llm = FakeLlm::new(vec![batch(vec![verdict(1, "Factures", 0.95)])]);

    let stats = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    assert_eq!(stats.moved, 1);
    assert_eq!(*mail.moves.borrow(), vec![(1, "Factures".to_string())]);
    assert!(mail.copies.borrow().is_empty());
}

#[tokio::test]
async fn uid_deja_traite_est_ignore_au_poll_suivant() {
    let (mut store, cfg) = setup();
    store.mark_processed("orange", "INBOX", 7, 1).unwrap();
    let mut mail = FakeMail::new(7);
    mail.add_mail(1, "edf@edf.fr", "Facture", "corps");
    mail.add_mail(2, "news@shop.fr", "Promo", "corps");
    let llm = FakeLlm::new(vec![batch(vec![verdict(2, "Newsletters", 0.9)])]);

    let stats = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    assert_eq!(stats.scanned, 2);
    assert_eq!(stats.pending, 1);
    assert_eq!(stats.decided, 1);
    let req = &llm.requests.borrow()[0];
    let user = &req.messages[1].content;
    assert!(user.contains("uid=2"));
    assert!(!user.contains("uid=1"));
}

#[tokio::test]
async fn uidvalidity_change_retraite_le_dossier() {
    let (mut store, cfg) = setup();
    store.mark_processed("orange", "INBOX", 1, 1).unwrap();
    let mut mail = FakeMail::new(2);
    mail.add_mail(1, "edf@edf.fr", "Facture", "corps");
    let llm = FakeLlm::new(vec![batch(vec![verdict(1, "Factures", 0.95)])]);

    let stats = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    assert_eq!(stats.pending, 1);
    assert_eq!(*mail.copies.borrow(), vec![(1, "Factures".to_string())]);
    assert!(store.is_processed("orange", "INBOX", 2, 1).unwrap());
}

#[tokio::test]
async fn batch_size_decoupe_les_appels_llm() {
    let (mut store, mut cfg) = setup();
    cfg.batch_size = 2;
    let mut mail = FakeMail::new(1);
    for uid in 1..=3 {
        mail.add_mail(uid, "a@b.fr", &format!("sujet {uid}"), "corps");
    }
    let llm = FakeLlm::new(vec![
        batch(vec![
            verdict(1, "Factures", 0.9),
            verdict(2, "Newsletters", 0.9),
        ]),
        batch(vec![verdict(3, "Factures", 0.9)]),
    ]);

    let stats = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    assert_eq!(stats.decided, 3);
    assert_eq!(llm.requests.borrow().len(), 2);
    assert_eq!(llm.schemas.borrow().len(), 2);
    assert!(llm.schemas.borrow()[0].contains("verdicts"));
    assert_eq!(BATCH_SCHEMA, llm.schemas.borrow()[0].as_str());
}

#[tokio::test]
async fn aucun_mail_non_traite_ne_appelle_pas_le_llm() {
    let (mut store, cfg) = setup();
    let mut mail = FakeMail::new(1);
    let llm = FakeLlm::new(vec![]);

    let stats = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    assert_eq!(stats.scanned, 0);
    assert_eq!(stats.pending, 0);
    assert!(llm.requests.borrow().is_empty());
}

#[tokio::test]
async fn uid_sans_verdict_fait_echouer_le_batch_sans_rien_marquer() {
    let (mut store, cfg) = setup();
    let mut mail = FakeMail::new(1);
    mail.add_mail(1, "a@b.fr", "s1", "corps");
    mail.add_mail(2, "b@c.fr", "s2", "corps");
    let llm = FakeLlm::new(vec![batch(vec![
        verdict(1, "Factures", 0.9),
        verdict(99, "Newsletters", 0.9),
    ])]);

    let err = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Llm(_)));
    assert!(!store.is_processed("orange", "INBOX", 1, 1).unwrap());
    assert!(!store.is_processed("orange", "INBOX", 1, 2).unwrap());
}

#[tokio::test]
async fn le_prompt_embarque_la_whitelist_les_profils_et_le_system_prompt() {
    let (mut store, cfg) = setup();
    let mut mail = FakeMail::new(1);
    mail.add_mail(1, "edf@edf.fr", "Facture octobre", "corps de la facture");
    let llm = FakeLlm::new(vec![batch(vec![verdict(1, "Factures", 0.95)])]);

    run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    let req = &llm.requests.borrow()[0];
    assert_eq!(req.model, "modele-test");
    assert_eq!(req.messages[0].role, "system");
    assert_eq!(req.messages[0].content, "PROMPT SYSTEME DE TEST");
    let user = &req.messages[1].content;
    assert!(user.contains("Factures"));
    assert!(user.contains("factures et justificatifs"));
    assert!(user.contains("Facture EDF octobre"));
    assert!(user.contains("Newsletters"));
    assert!(user.contains("uid=1"));
    assert!(user.contains("edf@edf.fr"));
    assert!(user.contains("Facture octobre"));
    assert!(user.contains("corps de la facture"));
}

#[tokio::test]
async fn le_prompt_borne_les_exemples_par_dossier_a_examples_in_prompt() {
    let (mut store, mut cfg) = setup();
    cfg.examples_in_prompt = 2;
    cfg.profiles[0].examples = vec![
        "ex-1".into(),
        "ex-2".into(),
        "ex-3".into(),
        "ex-4".into(),
        "ex-5".into(),
    ];
    let mut mail = FakeMail::new(1);
    mail.add_mail(1, "edf@edf.fr", "Facture octobre", "corps");
    let llm = FakeLlm::new(vec![batch(vec![verdict(1, "Factures", 0.95)])]);

    run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap();

    let user = &llm.requests.borrow()[0].messages[1].content;
    assert!(user.contains("ex-1") && user.contains("ex-2"));
    assert!(
        !user.contains("ex-3") && !user.contains("ex-4") && !user.contains("ex-5"),
        "exemples bornes a examples_in_prompt=2: {user}"
    );
}

#[tokio::test]
async fn verdict_orphelin_seul_fait_echouer_le_batch() {
    let (mut store, cfg) = setup();
    let mut mail = FakeMail::new(1);
    mail.add_mail(1, "a@b.fr", "s1", "corps");
    let llm = FakeLlm::new(vec![batch(vec![
        verdict(1, "Factures", 0.9),
        verdict(99, "Newsletters", 0.9),
    ])]);

    let err = run(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg)
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Llm(_)));
    assert!(err.to_string().contains("orphelin"));
    assert!(!store.is_processed("orange", "INBOX", 1, 1).unwrap());
    assert!(mail.copies.borrow().is_empty());
}

#[test]
fn corps_long_tronque_au_seuil_avec_marqueur() {
    let (mut store, mut cfg) = setup();
    cfg.max_body_chars = 50;
    let body = "a".repeat(200);
    let mut mail = FakeMail::new(1);
    mail.add_mail(1, "a@b.fr", "s1", &body);
    let llm = FakeLlm::new(vec![batch(vec![verdict(1, "Factures", 0.9)])]);
    let _ = run_tokio(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg);

    let user = &llm.requests.borrow()[0].messages[1].content;
    assert!(user.contains("… [tronqué]"), "marqueur absent: {user}");
    assert!(!user.contains(&"a".repeat(200)), "corps non tronque");
    let borne = "a".repeat(50);
    assert!(user.contains(&borne));
}

#[test]
fn corps_court_intact_sans_marqueur() {
    let (mut store, cfg) = setup();
    let mut mail = FakeMail::new(1);
    mail.add_mail(1, "a@b.fr", "s1", "corps court");
    let llm = FakeLlm::new(vec![batch(vec![verdict(1, "Factures", 0.9)])]);
    let _ = run_tokio(&mut mail, &llm, &mut store, "orange", "INBOX", &cfg);

    let user = &llm.requests.borrow()[0].messages[1].content;
    assert!(user.contains("corps court"));
    assert!(!user.contains("tronqué"));
}

fn run_tokio(
    mail: &mut FakeMail,
    llm: &FakeLlm,
    store: &mut Store,
    mailbox: &str,
    folder: &str,
    cfg: &PipelineConfig,
) -> RunStats {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(run(mail, llm, store, mailbox, folder, cfg))
        .unwrap()
}

#[test]
fn pipeline_config_for_propage_le_flag_dry_run_explicite() {
    // prompt ecrit dans un fichier temporaire : le test ne depend pas de la
    // disposition des fichiers du depot (retour de revue PR #9).
    let dir = std::env::temp_dir().join(format!(
        "ez-test-prompt-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let prompt_path = dir.join("prompt.md");
    std::fs::write(&prompt_path, "PROMPT DE TEST").unwrap();

    let toml_str = format!(
        r#"
[[mailbox]]
id = "orange"
host = "127.0.0.1"
port = 1143
username = "a@b.c"
password_env = "EZ_PW"

[llm]
base_url = "http://localhost:4000/v1"
api_key_env = "EZ_LLM_API_KEY"
model = "local"
max_body_chars = 1234

[triage]
prompt_path = {:?}
"#,
        prompt_path.to_str().unwrap()
    );
    let cfg = email_zero::config::Config::parse(&toml_str).unwrap();
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["orange".to_string()]).unwrap();
    store
        .upsert_folders("orange", &[("Factures", None, false)])
        .unwrap();

    let pc_on = email_zero::pipeline::pipeline_config_for(&cfg, &store, "orange", true).unwrap();
    assert!(pc_on.dry_run);
    assert_eq!(pc_on.max_body_chars, 1234);
    assert_eq!(pc_on.whitelist, vec!["Factures".to_string()]);
    assert_eq!(pc_on.system_prompt, "PROMPT DE TEST");

    let pc_off = email_zero::pipeline::pipeline_config_for(&cfg, &store, "orange", false).unwrap();
    assert!(!pc_off.dry_run);

    let _ = std::fs::remove_dir_all(&dir);
}
