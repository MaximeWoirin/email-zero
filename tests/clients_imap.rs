use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use email_zero::clients::imap::ImapMailClient;
use email_zero::config::RetryConfig;
use email_zero::ports::MailClient;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAIL_1: &str = "From: Alice <alice@example.com>\r\nTo: bob@example.com\r\nSubject: Facture octobre\r\nDate: Wed, 01 Nov 2026 10:00:00 +0100\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nVotre facture EDF.\r\n";
const MAIL_2: &str = "From: news@shop.fr\r\nTo: bob@example.com\r\nSubject: Promo\r\nDate: Wed, 01 Nov 2026 11:00:00 +0100\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n-50% partout.\r\n";

/// Mini serveur IMAP en clair sur loopback : gere LOGIN, SELECT, UID SEARCH,
/// UID FETCH, UID COPY, UID MOVE, LIST, STATUS, LOGOUT. Un seul mail non lu
/// (uid 1) et un lu (uid 2). Enregistre les commandes COPY/MOVE.
struct FakeImapState {
    copies: Vec<(u32, String)>,
    moves: Vec<(u32, String)>,
    fetch_queries: Vec<String>,
    verbs: Vec<String>,
}

async fn spawn_fake_imap() -> (u16, Arc<Mutex<FakeImapState>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(FakeImapState {
        copies: Vec::new(),
        moves: Vec::new(),
        fetch_queries: Vec::new(),
        verbs: Vec::new(),
    }));
    let state_out = state.clone();
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
                if buf.windows(2).any(|w| w == b"\r\n") && buf.ends_with(b"\r\n") {
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
            state_out.lock().unwrap().verbs.push(verb.clone());
            let reply = match verb.as_str() {
                "LOGIN" => format!("{tag} OK LOGIN ok\r\n"),
                "CAPABILITY" => {
                    format!("* CAPABILITY IMAP4rev1 UIDPLUS MOVE\r\n{tag} OK CAPABILITY ok\r\n")
                }
                "SELECT" | "EXAMINE" => {
                    let folder = rest.trim_matches('"').to_string();
                    let _ = folder;
                    format!(
                        "* 2 EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen)\r\n* OK [UIDVALIDITY 42] UIDs valides\r\n* OK [UIDNEXT 3] next\r\n{tag} OK SELECT ok\r\n"
                    )
                }
                "STATUS" => {
                    format!("* STATUS INBOX (UIDVALIDITY 42)\r\n{tag} OK STATUS ok\r\n")
                }
                "UID" => {
                    let (sub, args) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
                    match sub {
                        "SEARCH" => format!("* SEARCH 1\r\n{tag} OK SEARCH ok\r\n"),
                        "FETCH" => {
                            let uid: u32 = args
                                .split(' ')
                                .next()
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0);
                            let query = args.split_once(' ').map(|x| x.1).unwrap_or("").to_string();
                            state_out.lock().unwrap().fetch_queries.push(query);
                            let mail = if uid == 1 { MAIL_1 } else { MAIL_2 };
                            let body = mail.as_bytes();
                            format!(
                                "* 1 FETCH (UID {uid} BODY[] {{{}}}\r\n{mail})\r\n{tag} OK FETCH ok\r\n",
                                body.len()
                            )
                        }
                        "COPY" => {
                            let mut it = args.splitn(2, ' ');
                            let uid: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
                            let target = it.next().unwrap_or("").trim_matches('"').to_string();
                            state_out.lock().unwrap().copies.push((uid, target));
                            format!("{tag} OK COPY ok\r\n")
                        }
                        "MOVE" => {
                            let mut it = args.splitn(2, ' ');
                            let uid: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
                            let target = it.next().unwrap_or("").trim_matches('"').to_string();
                            state_out.lock().unwrap().moves.push((uid, target));
                            format!("{tag} OK MOVE ok\r\n")
                        }
                        _ => format!("{tag} BAD commande inconnue\r\n"),
                    }
                }
                "LIST" => {
                    format!(
                        "* LIST (\\HasNoChildren) \"/\" INBOX\r\n* LIST (\\HasNoChildren \\Sent) \"/\" Sent\r\n* LIST (\\HasNoChildren \\Trash) \"/\" Trash\r\n* LIST (\\HasNoChildren) \"/\" Factures\r\n{tag} OK LIST ok\r\n"
                    )
                }
                "NOOP" => format!("{tag} OK NOOP ok\r\n"),
                "LOGOUT" => {
                    let _ = sock
                        .write_all(
                            format!("* BYE deconnexion\r\n{tag} OK LOGOUT ok\r\n").as_bytes(),
                        )
                        .await;
                    let _ = sock.shutdown().await;
                    return;
                }
                _ => format!("{tag} BAD {verb}\r\n"),
            };
            if sock.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    (port, state)
}

async fn connect_client(port: u16) -> ImapMailClient {
    let mb = email_zero::config::MailboxConfig {
        id: "test".into(),
        host: "127.0.0.1".into(),
        port,
        username: "bob@example.com".into(),
        password_env: None,
        password_cmd: None,
        password_plain: Some("secret".into()),
        tls: false,
        poll_interval_secs: 300,
    };
    let password = mb.resolve_password_from_env().unwrap();
    let session = email_zero::clients::imap::connect(&mb, &password)
        .await
        .unwrap();
    ImapMailClient::new(
        session,
        RetryConfig {
            max_attempts: 1,
            backoff_base_secs: 0,
        },
    )
}

#[tokio::test]
async fn list_folders_retourne_noms_et_special_use() {
    let (port, _state) = spawn_fake_imap().await;
    let mut client = connect_client(port).await;

    let folders = client.list_folders().await.unwrap();

    let by_name: HashMap<_, _> = folders
        .iter()
        .map(|f| (f.name.as_str(), f.special_use.as_deref()))
        .collect();
    assert_eq!(by_name.get("INBOX"), Some(&None));
    assert_eq!(by_name.get("Sent"), Some(&Some("\\Sent")));
    assert_eq!(by_name.get("Trash"), Some(&Some("\\Trash")));
    assert_eq!(by_name.get("Factures"), Some(&None));
}

#[tokio::test]
async fn uidvalidity_vient_du_select_sans_status() {
    let (port, state) = spawn_fake_imap().await;
    let mut client = connect_client(port).await;

    // UIDVALIDITY portee par la reponse SELECT (RFC 3501 §6.3.1), zero STATUS
    assert_eq!(client.uidvalidity("INBOX").await.unwrap(), 42);
    // un second appel sur le meme dossier ne re-SELECT ni ne STATUS
    assert_eq!(client.uidvalidity("INBOX").await.unwrap(), 42);

    let st = state.lock().unwrap();
    assert!(
        !st.verbs.iter().any(|v| v == "STATUS"),
        "aucune commande STATUS ne doit etre emise : {:?}",
        st.verbs
    );
    assert_eq!(
        st.verbs.iter().filter(|v| *v == "SELECT").count(),
        1,
        "un seul SELECT (mis en cache) : {:?}",
        st.verbs
    );
}

#[tokio::test]
async fn fetch_headers_utilise_body_peek_header() {
    let (port, state) = spawn_fake_imap().await;
    let mut client = connect_client(port).await;

    let h = client.fetch_headers("INBOX", 1).await.unwrap();

    assert_eq!(h.subject, "Facture octobre");
    assert!(h.from.contains("alice@example.com"));
    let st = state.lock().unwrap();
    assert!(
        st.fetch_queries.iter().any(|q| q == "BODY.PEEK[HEADER]"),
        "fetch_headers doit emettre BODY.PEEK[HEADER] : {:?}",
        st.fetch_queries
    );
    assert!(
        !st.fetch_queries.iter().any(|q| q == "BODY.PEEK[]"),
        "fetch_headers ne doit PAS telecharger le corps complet : {:?}",
        st.fetch_queries
    );
}

#[tokio::test]
async fn search_unseen_retourne_les_uid_tries() {
    let (port, _state) = spawn_fake_imap().await;
    let mut client = connect_client(port).await;
    assert_eq!(client.search_unseen("INBOX").await.unwrap(), vec![1]);
}

#[tokio::test]
async fn fetch_body_parse_le_mail_rfc5322() {
    let (port, _state) = spawn_fake_imap().await;
    let mut client = connect_client(port).await;

    let mail = client.fetch_body("INBOX", 1).await.unwrap();

    assert_eq!(mail.header.subject, "Facture octobre");
    assert_eq!(mail.header.from, "Alice <alice@example.com>");
    assert!(mail.header.date.starts_with("2026-11-01T10:00:00"));
    assert!(mail.body_text.contains("facture EDF"));
    let h = client.fetch_headers("INBOX", 1).await.unwrap();
    assert_eq!(h.subject, "Facture octobre");
}

#[tokio::test]
async fn fetch_utilise_body_peek_pour_ne_jamais_poser_seen() {
    let (port, state) = spawn_fake_imap().await;
    let mut client = connect_client(port).await;

    client.fetch_body("INBOX", 1).await.unwrap();

    let st = state.lock().unwrap();
    assert_eq!(st.fetch_queries, vec!["BODY.PEEK[]".to_string()]);
}

#[tokio::test]
async fn copy_et_move_appellent_uid_copy_et_uid_move() {
    let (port, state) = spawn_fake_imap().await;
    let mut client = connect_client(port).await;

    client.copy("INBOX", 1, "Factures").await.unwrap();
    client.r#move("INBOX", 1, "Archive").await.unwrap();

    let st = state.lock().unwrap();
    assert_eq!(st.copies, vec![(1, "Factures".to_string())]);
    assert_eq!(st.moves, vec![(1, "Archive".to_string())]);
}

#[tokio::test]
async fn connexion_refusee_remique_une_erreur_imap() {
    let mb = email_zero::config::MailboxConfig {
        id: "test".into(),
        host: "127.0.0.1".into(),
        port: 1,
        username: "bob@example.com".into(),
        password_env: Some("EZ_NOPE".into()),
        password_cmd: None,
        password_plain: None,
        tls: false,
        poll_interval_secs: 300,
    };
    let err = email_zero::clients::imap::connect(&mb, "x")
        .await
        .unwrap_err();
    assert!(
        matches!(err, email_zero::error::Error::Imap(_)),
        "err: {err}"
    );
}

#[allow(dead_code)]
fn _unused(_s: TcpStream) {}
