use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use email_zero::clients::llm::OpenAiLlmClient;
use email_zero::config::{LlmConfig, RetryConfig};
use email_zero::error::Error;
use email_zero::pipeline::BATCH_SCHEMA;
use email_zero::ports::{ChatMessage, JsonChatRequest, LlmClient};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Deserialize, PartialEq)]
struct Verdict {
    uid: u64,
    category: String,
    priority: i64,
    summary: String,
    is_newsletter: bool,
    confidence: f32,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Batch {
    verdicts: Vec<Verdict>,
}

fn llm_cfg(base_url: &str, max_attempts: u32) -> LlmConfig {
    LlmConfig {
        base_url: base_url.to_string(),
        api_key_env: "EZ_TEST_KEY".into(),
        model: "modele-test".into(),
        temperature: 0.2,
        top_p: 1.0,
        max_tokens: 4096,
        reasoning_effort: Some("low".into()),
        timeout_secs: 5,
        batch_size: 15,
        max_body_chars: 2000,
        rate_limit_per_min: 0,
        retry: RetryConfig {
            max_attempts,
            backoff_base_secs: 0,
        },
    }
}

fn req() -> JsonChatRequest {
    JsonChatRequest {
        model: "modele-test".into(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: "PROMPT SYSTEME".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "batch de mails".into(),
            },
        ],
        temperature: 0.2,
        top_p: 1.0,
        max_tokens: 4096,
        response_format_json_schema: Some(BATCH_SCHEMA.into()),
    }
}

struct RecordedRequest {
    authorization: String,
    body: serde_json::Value,
}

/// Mini serveur HTTP OpenAI-compatible sur loopback : repond selon `script`
/// (codes HTTP), enregistre chaque requete recue.
async fn spawn_llm_server(
    script: Vec<u16>,
    content: String,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<RecordedRequest>>>,
    Arc<AtomicUsize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let recorded_task = Arc::new(std::sync::Mutex::new(Vec::new()));
    let counter = Arc::new(AtomicUsize::new(0));
    let content = Arc::new(content);
    let script = Arc::new(script);
    let recorded_out = recorded_task.clone();
    let counter_out = counter.clone();
    let content_out = content.clone();
    let script_out = script.clone();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let head_end = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let content_length = head
                .lines()
                .find_map(|l| {
                    let l = l.to_ascii_lowercase();
                    l.strip_prefix("content-length:")
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
            let body = buf[head_end..head_end + content_length].to_vec();
            let authorization = head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
                .unwrap_or_default();
            let idx = counter_out.fetch_add(1, Ordering::SeqCst);
            let status = script_out.get(idx).copied().unwrap_or(200);
            let payload = if status == 200 {
                let c = serde_json::to_string(content_out.as_str()).unwrap();
                format!(r#"{{"choices":[{{"message":{{"content":{c}}}}}]}}"#)
            } else {
                format!(r#"{{"error":{{"message":"panne simulee {status}"}}}}"#)
            };
            recorded_out.lock().unwrap().push(RecordedRequest {
                authorization,
                body: serde_json::from_slice(&body).unwrap(),
            });
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            );
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.shutdown().await.ok();
        }
    });
    (
        format!("http://127.0.0.1:{port}/v1"),
        recorded_task,
        counter,
    )
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[tokio::test]
async fn chat_json_schema_envoie_le_bon_payload_et_deserialise() {
    let content = r#"{"verdicts":[{"uid":1,"category":"Factures","priority":2,"summary":"resume","is_newsletter":false,"confidence":0.95}]}"#;
    let (base, recorded, counter) = spawn_llm_server(vec![200], content.to_string()).await;
    let cfg = llm_cfg(&base, 3);
    let client = OpenAiLlmClient::new(&cfg, &|name| {
        assert_eq!(name, "EZ_TEST_KEY");
        Some("cle-secrete-test".to_string())
    })
    .unwrap();

    let batch: Batch = client.chat_json_schema(req(), BATCH_SCHEMA).await.unwrap();

    assert_eq!(batch.verdicts.len(), 1);
    assert_eq!(batch.verdicts[0].uid, 1);
    assert_eq!(batch.verdicts[0].category, "Factures");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let rec = &recorded.lock().unwrap()[0];
    assert_eq!(rec.authorization, "Bearer cle-secrete-test");
    assert_eq!(rec.body["model"], "modele-test");
    assert_eq!(rec.body["messages"][0]["role"], "system");
    assert_eq!(rec.body["messages"][1]["content"], "batch de mails");
    assert_eq!(rec.body["reasoning_effort"], "low");
    assert_eq!(rec.body["response_format"]["type"], "json_schema");
    assert_eq!(
        rec.body["response_format"]["json_schema"]["name"],
        "triage_batch"
    );
}

#[tokio::test]
async fn retry_sur_erreur_http_puis_succes() {
    let content = r#"{"verdicts":[]}"#;
    let (base, _rec, counter) =
        spawn_llm_server_with_multi(vec![500, 500, 200], content.to_string()).await;
    let cfg = llm_cfg(&base, 3);
    let client = OpenAiLlmClient::new(&cfg, &|_| Some("k".into())).unwrap();

    let batch: Batch = client.chat_json_schema(req(), BATCH_SCHEMA).await.unwrap();

    assert!(batch.verdicts.is_empty());
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn erreur_4xx_hors_429_n_est_pas_retentee() {
    let (base, _rec, counter) =
        spawn_llm_server_with_multi(vec![400, 400, 400], String::new()).await;
    let cfg = llm_cfg(&base, 3);
    let client = OpenAiLlmClient::new(&cfg, &|_| Some("k".into())).unwrap();

    let err = match client.chat_json_schema::<Batch>(req(), BATCH_SCHEMA).await {
        Err(e) => e,
        Ok(_) => panic!("devrait echouer"),
    };

    assert!(matches!(err, Error::LlmNonRetryable(_)), "err: {err}");
    assert!(err.to_string().contains("400"), "err: {err}");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "un 400 ne doit pas etre retente"
    );
}

#[tokio::test]
async fn erreur_429_est_retentee() {
    let content = r#"{"verdicts":[]}"#;
    let (base, _rec, counter) =
        spawn_llm_server_with_multi(vec![429, 200], content.to_string()).await;
    let cfg = llm_cfg(&base, 3);
    let client = OpenAiLlmClient::new(&cfg, &|_| Some("k".into())).unwrap();

    let batch: Batch = client.chat_json_schema(req(), BATCH_SCHEMA).await.unwrap();

    assert!(batch.verdicts.is_empty());
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "le 429 doit etre retente"
    );
}

#[tokio::test]
async fn erreur_5xx_est_retentee_jusqu_au_max() {
    let (base, _rec, counter) =
        spawn_llm_server_with_multi(vec![500, 500, 500], String::new()).await;
    let cfg = llm_cfg(&base, 3);
    let client = OpenAiLlmClient::new(&cfg, &|_| Some("k".into())).unwrap();

    let err = match client.chat_json_schema::<Batch>(req(), BATCH_SCHEMA).await {
        Err(e) => e,
        Ok(_) => panic!("devrait echouer"),
    };

    assert!(matches!(err, Error::Llm(_)), "err: {err}");
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn chat_json_honore_response_format_json_schema_de_la_requete() {
    let content = r#"{"verdicts":[]}"#;
    let (base, recorded, _c) = spawn_llm_server(vec![200], content.to_string()).await;
    let cfg = llm_cfg(&base, 1);
    let client = OpenAiLlmClient::new(&cfg, &|_| Some("k".into())).unwrap();

    // chat_json (pas chat_json_schema) avec response_format_json_schema renseigne
    let mut r = req();
    r.response_format_json_schema = Some(BATCH_SCHEMA.into());
    let batch: Batch = client.chat_json(r).await.unwrap();

    assert!(batch.verdicts.is_empty());
    let rec = &recorded.lock().unwrap()[0];
    assert_eq!(rec.body["response_format"]["type"], "json_schema");
}

#[tokio::test]
async fn chat_json_sans_schema_n_envoie_pas_de_response_format() {
    let content = r#"{"verdicts":[]}"#;
    let (base, recorded, _c) = spawn_llm_server(vec![200], content.to_string()).await;
    let cfg = llm_cfg(&base, 1);
    let client = OpenAiLlmClient::new(&cfg, &|_| Some("k".into())).unwrap();

    let mut r = req();
    r.response_format_json_schema = None;
    let _: Batch = client.chat_json(r).await.unwrap();

    let rec = &recorded.lock().unwrap()[0];
    assert!(
        rec.body.get("response_format").is_none(),
        "pas de response_format attendu: {}",
        rec.body
    );
}

#[test]
fn base_url_http_non_loopback_est_refuse() {
    let err =
        email_zero::clients::llm::validate_base_url("http://litellm.example.com/v1").unwrap_err();
    assert!(err.to_string().contains("https://"), "err: {err}");
}

#[test]
fn base_url_sans_scheme_est_refuse() {
    assert!(email_zero::clients::llm::validate_base_url("litellm.example.com/v1").is_err());
}

#[test]
fn base_url_https_et_loopback_sont_acceptes() {
    assert!(email_zero::clients::llm::validate_base_url("https://litellm.example.com/v1").is_ok());
    assert!(email_zero::clients::llm::validate_base_url("http://127.0.0.1:4000/v1").is_ok());
    assert!(email_zero::clients::llm::validate_base_url("http://localhost:4000/v1").is_ok());
}

#[tokio::test]
async fn client_llm_refuse_une_base_url_http_distante() {
    let mut cfg = llm_cfg("http://litellm-distant.example.com/v1", 1);
    let err = match OpenAiLlmClient::new(&cfg, &|_| Some("k".into())) {
        Err(e) => e,
        Ok(_) => panic!("devrait refuser http non-loopback"),
    };
    assert!(err.to_string().contains("https://"), "err: {err}");
    cfg.base_url = "https://litellm.example.com/v1".into();
    assert!(OpenAiLlmClient::new(&cfg, &|_| Some("k".into())).is_ok());
}

#[tokio::test]
async fn cle_api_manquante_refuse_de_construire_le_client() {
    let cfg = llm_cfg("http://127.0.0.1:1/v1", 3);
    let err = match OpenAiLlmClient::new(&cfg, &|_| None) {
        Err(e) => e,
        Ok(_) => panic!("devrait echouer"),
    };
    assert!(matches!(err, Error::MissingSecretEnv(ref n) if n == "EZ_TEST_KEY"));
}

#[tokio::test]
async fn contenu_non_json_fait_echouer_la_deserialisation() {
    let (base, _rec, _c) = spawn_llm_server(vec![200], "pas du json".to_string()).await;
    let cfg = llm_cfg(&base, 1);
    let client = OpenAiLlmClient::new(&cfg, &|_| Some("k".into())).unwrap();

    let err = client
        .chat_json_schema::<Batch>(req(), BATCH_SCHEMA)
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Llm(_)), "err: {err}");
    assert!(err.to_string().contains("non deserialisable"), "err: {err}");
}

/// Variante acceptant plusieurs connexions successives (retry = plusieurs requetes).
async fn spawn_llm_server_with_multi(
    script: Vec<u16>,
    content: String,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<RecordedRequest>>>,
    Arc<AtomicUsize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
    let counter = Arc::new(AtomicUsize::new(0));
    let content = Arc::new(content);
    let script = Arc::new(script);
    let recorded_out = recorded.clone();
    let counter_out = counter.clone();
    let content_out = content.clone();
    let script_out = script.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let recorded = recorded_out.clone();
            let counter = counter_out.clone();
            let content = content_out.clone();
            let script = script_out.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let head_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
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
                let body = buf[head_end..head_end + content_length].to_vec();
                let authorization = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
                    .unwrap_or_default();
                let idx = counter.fetch_add(1, Ordering::SeqCst);
                let status = script.get(idx).copied().unwrap_or(200);
                let payload = if status == 200 {
                    let c = serde_json::to_string(content.as_str()).unwrap();
                    format!(r#"{{"choices":[{{"message":{{"content":{c}}}}}]}}"#)
                } else {
                    format!(r#"{{"error":{{"message":"panne simulee {status}"}}}}"#)
                };
                recorded.lock().unwrap().push(RecordedRequest {
                    authorization,
                    body: serde_json::from_slice(&body).unwrap(),
                });
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                sock.write_all(response.as_bytes()).await.unwrap();
                sock.shutdown().await.ok();
            });
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), recorded, counter)
}

#[allow(dead_code)]
fn _garde_refcell(_r: &RefCell<u32>) {}
