use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use email_zero::clients::imap::{connect, ImapMailClient};
use email_zero::clients::llm::OpenAiLlmClient;
use email_zero::config::{Config, RetryConfig};
use email_zero::pipeline::{pipeline_config_for, run};
use email_zero::store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MAIL_1: &str = "From: edf@edf.fr\r\nTo: bob@example.com\r\nSubject: Facture octobre\r\nDate: Wed, 01 Nov 2026 10:00:00 +0100\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nVotre facture EDF est disponible.\r\n";

#[derive(Default)]
struct ImapLog {
    copies: Vec<(u32, String)>,
}

async fn spawn_fake_imap() -> (u16, Arc<Mutex<ImapLog>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(ImapLog::default()));
    let log_out = log.clone();
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
                "STATUS" => format!("* STATUS INBOX (UIDVALIDITY 42)\r\n{tag} OK STATUS ok\r\n"),
                "UID" => {
                    let (sub, args) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
                    match sub {
                        "SEARCH" => format!("* SEARCH 1\r\n{tag} OK SEARCH ok\r\n"),
                        "FETCH" => {
                            let uid: u32 =
                                args.split(' ').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                            let body = MAIL_1.as_bytes();
                            format!(
                                "* 1 FETCH (UID {uid} BODY[] {{{}}}\r\n{MAIL_1})\r\n{tag} OK FETCH ok\r\n",
                                body.len()
                            )
                        }
                        "COPY" => {
                            let mut it = args.splitn(2, ' ');
                            let uid: u32 = it.next().unwrap_or("0").parse().unwrap_or(0);
                            let target = it.next().unwrap_or("").trim_matches('"').to_string();
                            log_out.lock().unwrap().copies.push((uid, target));
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
    (port, log)
}

async fn spawn_fake_llm(content: String) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_out = counter.clone();
    let content = Arc::new(content);
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
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
        counter_out.fetch_add(1, Ordering::SeqCst);
        let c = serde_json::to_string(content.as_str()).unwrap();
        let payload = format!(r#"{{"choices":[{{"message":{{"content":{c}}}}}]}}"#);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        sock.write_all(response.as_bytes()).await.unwrap();
        sock.shutdown().await.ok();
    });
    (format!("http://127.0.0.1:{port}/v1"), counter)
}

fn test_config(imap_port: u16, llm_base: &str, prompt_path: &str) -> Config {
    let toml = format!(
        r#"
[[mailbox]]
id = "test"
host = "127.0.0.1"
port = {imap_port}
username = "bob@example.com"
password_env = "EZ_TEST_PW"
tls = false
poll_interval_secs = 1

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
prompt_path = "{prompt_path}"

[store]
path = ":memory:"
"#
    );
    Config::parse(&toml).unwrap()
}

#[tokio::test]
async fn end_to_end_client_imap_reel_client_llm_reel_pipeline_complet() {
    let (imap_port, imap_log) = spawn_fake_imap().await;
    let verdict = r#"{"verdicts":[{"uid":1,"category":"Factures","priority":1,"summary":"Facture EDF octobre","is_newsletter":false,"confidence":0.97}]}"#;
    let (llm_base, llm_calls) = spawn_fake_llm(verdict.to_string()).await;

    let dir = std::env::temp_dir().join(format!(
        "ez-e2e-prompt-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let prompt_path = dir.join("triage.md");
    std::fs::write(&prompt_path, "Tu es l'assistant de tri Email Zero.").unwrap();
    let prompt_path = prompt_path.to_str().unwrap().to_string();

    let cfg = test_config(imap_port, &llm_base, &prompt_path);

    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["test".to_string()]).unwrap();
    store
        .upsert_folders("test", &[("Factures", None, false)])
        .unwrap();

    let mb = &cfg.mailboxes[0];
    let password = mb
        .resolve_password(&|name| {
            assert_eq!(name, "EZ_TEST_PW");
            Some("mot-de-passe-test".to_string())
        })
        .unwrap();
    let session = connect(mb, &password).await.unwrap();
    let mut mail = ImapMailClient::new(
        session,
        RetryConfig {
            max_attempts: 1,
            backoff_base_secs: 0,
        },
    );
    let llm = OpenAiLlmClient::new(&cfg.llm, &|name| {
        assert_eq!(name, "EZ_TEST_KEY");
        Some("cle-test".to_string())
    })
    .unwrap();

    let pc = pipeline_config_for(&cfg, &store, "test", false).unwrap();
    assert_eq!(pc.whitelist, vec!["Factures".to_string()]);

    let stats = run(&mut mail, &llm, &mut store, "test", "INBOX", &pc)
        .await
        .unwrap();

    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.pending, 1);
    assert_eq!(stats.decided, 1);
    assert_eq!(stats.copied, 1);
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        imap_log.lock().unwrap().copies,
        vec![(1, "Factures".to_string())]
    );
    assert!(store.is_processed("test", "INBOX", 42, 1).unwrap());
    let r = store
        .triage_result("test", "INBOX", 42, 1)
        .unwrap()
        .unwrap();
    assert_eq!(r.target_folder.as_deref(), Some("Factures"));
    assert_eq!(r.action, "copy");
    assert_eq!(r.summary, "Facture EDF octobre");

    let _ = std::fs::remove_dir_all(&dir);
}
