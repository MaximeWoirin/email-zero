//! Tests de resilience de la connexion IMAP (issue #30) : read timeout sur les
//! lectures bloquees, detection rapide d'une fermeture serveur sans close_notify,
//! et reconnexion avec reprise en cours de poll (runner) sans doublon de traitement.

mod common;

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use email_zero::clients::imap::{connect, BoxSession, ImapMailClient};
use email_zero::config::{Config, MailboxConfig, RetryConfig};
use email_zero::error::{Error, Result};
use email_zero::ports::{JsonChatRequest, LlmClient, MailClient};
use email_zero::runner::{poll_mailbox, ConnectFn, PollOptions};
use email_zero::store::Store;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Etat partage du fake serveur IMAP : connexions acceptees, commandes vues,
/// copies enregistrees, NOOP recus.
#[derive(Default)]
struct ServerState {
    connects: usize,
    verbs: Vec<String>,
    copies: Vec<(u32, String)>,
    noops: usize,
}

/// Fake serveur IMAP en clair sur loopback, multi-connexions (pour simuler la
/// reconnexion). `cut = Some((n, sous_chaine))` : a la connexion n (0 = toutes),
/// des qu'une commande contient la sous-chaîne, le socket est ferme brutalement
/// (pas de BYE, pas de close_notify TLS — comme Orange en smoke). `hang_on` :
/// commande a laquelle le serveur ne repond jamais (lecture bloquee cote client).
struct FakeImapServer {
    port: u16,
    state: Arc<Mutex<ServerState>>,
}

fn mail_for(uid: u32) -> String {
    format!(
        "From: emetteur{uid}@example.com\r\nTo: bob@example.com\r\nSubject: Mail {uid}\r\n\
         Date: Wed, 01 Nov 2026 10:00:00 +0100\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         Corps du mail {uid}.\r\n"
    )
}

async fn spawn_fake_imap(
    unseen: Vec<u32>,
    cut: Option<(usize, String)>,
    hang_on: Option<String>,
) -> FakeImapServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(ServerState::default()));
    let st = state.clone();
    let unseen_c = unseen.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => return,
            };
            let conn_id = {
                let mut s = st.lock().unwrap();
                s.connects += 1;
                s.connects
            };
            let st = st.clone();
            let unseen = unseen_c.clone();
            let cut = cut.clone();
            let hang_on = hang_on.clone();
            tokio::spawn(async move {
                if sock.write_all(b"* OK fake imap pret\r\n").await.is_err() {
                    return;
                }
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    buf.clear();
                    loop {
                        let n = match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
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
                    st.lock().unwrap().verbs.push(verb.clone());
                    if let Some((n, sub)) = &cut {
                        if (*n == 0 || conn_id == *n) && line.contains(sub.as_str()) {
                            // fermeture brutale : drop du socket sans close_notify
                            return;
                        }
                    }
                    if let Some(sub) = &hang_on {
                        if line.contains(sub.as_str()) {
                            // ne repond jamais (le client doit timeouter)
                            tokio::time::sleep(Duration::from_secs(60)).await;
                            return;
                        }
                    }
                    let reply = match verb.as_str() {
                        "LOGIN" => format!("{tag} OK LOGIN ok\r\n"),
                        "CAPABILITY" => {
                            format!("* CAPABILITY IMAP4rev1 UIDPLUS MOVE\r\n{tag} OK CAPABILITY ok\r\n")
                        }
                        "SELECT" | "EXAMINE" => format!(
                            "* 2 EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen)\r\n* OK [UIDVALIDITY 42] UIDs valides\r\n* OK [UIDNEXT 3] next\r\n{tag} OK SELECT ok\r\n"
                        ),
                        "NOOP" => {
                            st.lock().unwrap().noops += 1;
                            format!("{tag} OK NOOP ok\r\n")
                        }
                        "UID" => {
                            let (sub, args) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
                            match sub {
                                "SEARCH" => {
                                    let list = unseen
                                        .iter()
                                        .map(|u| u.to_string())
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    format!("* SEARCH {list}\r\n{tag} OK SEARCH ok\r\n")
                                }
                                "FETCH" => {
                                    let (uid, query) = common::split_fetch_args(args);
                                    common::fetch_reply(&tag, uid, query, &mail_for(uid))
                                }
                                "COPY" => {
                                    let mut it = args.splitn(2, ' ');
                                    let uid: u32 =
                                        it.next().unwrap_or("0").parse().unwrap_or(0);
                                    let target =
                                        it.next().unwrap_or("").trim_matches('"').to_string();
                                    st.lock().unwrap().copies.push((uid, target));
                                    format!("{tag} OK COPY ok\r\n")
                                }
                                "MOVE" => {
                                    let mut it = args.splitn(2, ' ');
                                    let uid: u32 =
                                        it.next().unwrap_or("0").parse().unwrap_or(0);
                                    let target =
                                        it.next().unwrap_or("").trim_matches('"').to_string();
                                    st.lock().unwrap().copies.push((uid, target));
                                    format!("{tag} OK MOVE ok\r\n")
                                }
                                _ => format!("{tag} BAD commande inconnue\r\n"),
                            }
                        }
                        "LIST" => {
                            format!("* LIST (\\HasNoChildren) \"/\" INBOX\r\n{tag} OK LIST ok\r\n")
                        }
                        "LOGOUT" => {
                            let _ = sock
                                .write_all(
                                    format!("* BYE deconnexion\r\n{tag} OK LOGOUT ok\r\n")
                                        .as_bytes(),
                                )
                                .await;
                            return;
                        }
                        _ => format!("{tag} BAD {verb}\r\n"),
                    };
                    if sock.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    FakeImapServer { port, state }
}

fn mailbox(port: u16) -> MailboxConfig {
    MailboxConfig {
        id: "test".into(),
        host: "127.0.0.1".into(),
        port,
        username: "bob@example.com".into(),
        password_env: None,
        password_cmd: None,
        password_plain: Some("secret".into()),
        tls: false,
        poll_interval_secs: 60,
    }
}

async fn client(port: u16, read_timeout: Duration) -> ImapMailClient {
    let mb = mailbox(port);
    let password = mb.resolve_password_from_env().unwrap();
    let session = connect(&mb, &password).await.unwrap();
    ImapMailClient::new(
        session,
        RetryConfig {
            max_attempts: 1,
            backoff_base_secs: 0,
        },
        read_timeout,
    )
}

#[tokio::test]
async fn read_timeout_sur_lecture_bloquee_remonte_connection_lost_vite() {
    // le serveur ne repond jamais a UID SEARCH : sans timeout, la lecture
    // bloquerait jusqu'aux timers TCP (~4 min 30 observees en smoke, issue #30)
    let srv = spawn_fake_imap(vec![1, 2], None, Some("UID SEARCH".into())).await;
    let mut c = client(srv.port, Duration::from_secs(1)).await;

    let start = Instant::now();
    let err = c.search_unseen("INBOX").await.unwrap_err();

    assert!(matches!(err, Error::ImapConnectionLost(_)), "err: {err}");
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "le timeout de 1s doit couper la lecture, pas les timers TCP : {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn fermeture_serveur_sans_close_notify_detectee_immediatement() {
    // le serveur ferme brutalement le socket au premier UID SEARCH : le EOF
    // doit etre detecte tout de suite (read_timeout regle a 30s pour prouver
    // que c'est le EOF qui leve l'erreur, pas le timeout)
    let srv = spawn_fake_imap(vec![1, 2], Some((1, "UID SEARCH".into())), None).await;
    let mut c = client(srv.port, Duration::from_secs(30)).await;
    assert_eq!(c.uidvalidity("INBOX").await.unwrap(), 42);

    let start = Instant::now();
    let err = c.search_unseen("INBOX").await.unwrap_err();

    assert!(matches!(err, Error::ImapConnectionLost(_)), "err: {err}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "EOF detecte sans attendre le timeout de 30s : {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn noop_est_emis_sur_la_session() {
    let srv = spawn_fake_imap(vec![1], None, None).await;
    let mut c = client(srv.port, Duration::from_secs(5)).await;

    c.noop().await.unwrap();

    let st = srv.state.lock().unwrap();
    assert!(st.verbs.iter().any(|v| v == "NOOP"));
    assert_eq!(st.noops, 1);
}

/// Fake LLM scripté (une reponse JSON par appel, dans l'ordre).
struct FakeLlm {
    responses: Mutex<VecDeque<serde_json::Value>>,
}

impl LlmClient for FakeLlm {
    async fn chat_json<T: DeserializeOwned>(&self, _req: JsonChatRequest) -> Result<T> {
        let v = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| Error::Llm("fake llm: plus de reponse scriptee".into()))?;
        Ok(serde_json::from_value(v)?)
    }
    async fn chat_json_schema<T: DeserializeOwned>(
        &self,
        req: JsonChatRequest,
        _schema: &str,
    ) -> Result<T> {
        self.chat_json(req).await
    }
}

fn verdict(uid: u64) -> serde_json::Value {
    serde_json::json!({
        "uid": uid,
        "priority": 2,
        "summary": format!("resume uid {uid}"),
        "is_newsletter": false,
        "candidates": [{ "category": "Factures", "confidence": 0.99 }],
    })
}

fn batch_for(uids: &[u32]) -> serde_json::Value {
    serde_json::json!({ "verdicts": uids.iter().map(|u| verdict(*u as u64)).collect::<Vec<_>>() })
}

fn poll_config_toml(port: u16, prompt_path: &str, max_reconnects: u32) -> Config {
    let toml = format!(
        r#"
[[mailbox]]
id = "test"
host = "127.0.0.1"
port = {port}
username = "bob@example.com"
password = "secret"
tls = false
poll_interval_secs = 60

[llm]
base_url = "http://127.0.0.1:1/v1"
api_key_env = "EZ_TEST_KEY"
model = "m"
batch_size = 15

[imap]
timeout_secs = 5
keepalive_secs = 0
max_reconnects = {max_reconnects}

[imap.retry]
max_attempts = 1
backoff_base_secs = 0

[triage]
action = "copy"
confidence_threshold = 0.8
prompt_path = "{prompt_path}"

[store]
path = ":memory:"
"#
    );
    Config::parse(&toml).unwrap()
}

fn write_temp_prompt() -> String {
    let path = std::env::temp_dir().join(format!("ez-issue30-prompt-{}.md", std::process::id()));
    std::fs::write(&path, "PROMPT SYSTEME DE TEST").unwrap();
    path.to_string_lossy().to_string()
}

fn connect_counting(calls: Arc<AtomicUsize>) -> Box<ConnectFn> {
    Box::new(move |mb: MailboxConfig, pw: String| {
        calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { connect(&mb, &pw).await })
            as Pin<Box<dyn std::future::Future<Output = Result<BoxSession>> + Send>>
    })
}

#[tokio::test]
async fn poll_reprend_apres_coupure_sans_doublon_grace_au_state_store() {
    // 30 mails non lus, batchs de 15. Le serveur ferme brutalement la connexion
    // n°1 au premier FETCH du 2e batch (uid 16) : le poll doit se interrompre,
    // reconnecter (connexion n°2), et reprendre UNIQUEMENT les uids non traites.
    let unseen: Vec<u32> = (1..=30).collect();
    let srv = spawn_fake_imap(unseen.clone(), Some((1, "UID FETCH 16".into())), None).await;
    let prompt = write_temp_prompt();
    let cfg = poll_config_toml(srv.port, &prompt, 3);

    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["test".to_string()]).unwrap();
    store
        .upsert_folders("test", &[("Factures", Some("factures"), false)])
        .unwrap();

    let llm = FakeLlm {
        responses: Mutex::new(VecDeque::from(vec![
            batch_for(&unseen[..15]),
            batch_for(&unseen[15..]),
        ])),
    };

    let calls = Arc::new(AtomicUsize::new(0));
    let connect_fn = connect_counting(calls.clone());
    let pw = |_name: &str| None;

    let stats = poll_mailbox(
        &cfg,
        &llm,
        &mut store,
        &cfg.mailboxes[0],
        PollOptions {
            dry_run: false,
            discover: false,
        },
        &*connect_fn,
        &pw,
    )
    .await
    .expect("le poll doit reussir apres reconnexion");

    // deux connexions : l'initiale (coupee) + la reconnexion
    assert_eq!(calls.load(Ordering::SeqCst), 2, "une reconnexion attendue");
    // le run repris ne traite que le 2e batch (le 1er etait deja marque processed)
    assert_eq!(stats.pending, 15);
    assert_eq!(stats.decided, 15);

    // aucun doublon : 30 copies au total, chaque uid exactement une fois
    let mut copied_uids: Vec<u32> = srv
        .state
        .lock()
        .unwrap()
        .copies
        .iter()
        .map(|(u, _)| *u)
        .collect();
    copied_uids.sort_unstable();
    assert_eq!(copied_uids, unseen, "chaque mail copie exactement une fois");
    assert!(srv
        .state
        .lock()
        .unwrap()
        .copies
        .iter()
        .all(|(_, t)| t == "Factures"));

    // le state store a marque les 30 uids traites
    for uid in &unseen {
        assert!(
            store.is_processed("test", "INBOX", 42, *uid).unwrap(),
            "uid {uid} marque processed"
        );
    }
}

#[tokio::test]
async fn poll_echoue_definitivement_apres_max_reconnects() {
    // le serveur coupe a CHAQUE connexion des le UID SEARCH : avec
    // max_reconnects = 1, le poll tente 1 connexion + 1 reconnexion puis echoue
    let srv = spawn_fake_imap(vec![1, 2], Some((0, "UID SEARCH".into())), None).await;
    let prompt = write_temp_prompt();
    let cfg = poll_config_toml(srv.port, &prompt, 1);

    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["test".to_string()]).unwrap();
    store
        .upsert_folders("test", &[("Factures", None, false)])
        .unwrap();

    let llm = FakeLlm {
        responses: Mutex::new(VecDeque::new()),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let connect_fn = connect_counting(calls.clone());
    let pw = |_name: &str| None;

    let err = poll_mailbox(
        &cfg,
        &llm,
        &mut store,
        &cfg.mailboxes[0],
        PollOptions {
            dry_run: false,
            discover: false,
        },
        &*connect_fn,
        &pw,
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, Error::ImapConnectionLost(_)),
        "l'echec final est une perte de connexion: {err}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "1 essai initial + max_reconnects=1 reconnexion"
    );
}
