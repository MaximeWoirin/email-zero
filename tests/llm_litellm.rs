//! Etage E2E LLM (issue #22) : le vrai client LLM (`OpenAiLlmClient`) contre le
//! vrai proxy LiteLLM utilise en production — conteneur sur le loopback uniquement,
//! configure avec `mock_response` (config commitee `tests/litellm/config.yaml`).
//! Surface d'API reelle (authentification bearer, codes d'erreur natifs du proxy,
//! format de reponse) sans fournisseur, sans reseau externe, sans secret reel ;
//! contenu deterministe, un modele moque par scenario.
//!
//! Les scenarios `#[ignore]` ne tournent que dans le job CI `llm-litellm`
//! (`.github/workflows/ci.yml`), qui demarre le sidecar et renseigne
//! EZ_TEST_LITELLM_BASE_URL / EZ_TEST_LITELLM_API_KEY. `cargo test` standard les
//! saute. Ce n'est ni le fake in-process ni le mini-serveur loopback
//! (`tests/clients_llm.rs`, assertions sur les requetes emises) : c'est l'arbitre
//! qui attrape en CI les divergences fake/proxy reel — la reponse du LLM peut
//! devier du contrat (uid en double, uid omis, cf. smoke jalon 8 et issue #21)
//! sans que les tests actuels le voient.

use email_zero::clients::llm::OpenAiLlmClient;
use email_zero::config::{LlmConfig, RetryConfig};
use email_zero::error::Error;
use email_zero::pipeline::BATCH_SCHEMA;
use email_zero::ports::{ChatMessage, JsonChatRequest, LlmClient};
use serde::Deserialize;

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

/// La config commitee du sidecar doit rester synchronisee avec les scenarios testes
/// en `#[ignore]` : test rapide, zero reseau, tourne dans le `cargo test` standard.
#[test]
fn config_litellm_couvre_les_scenarios_e2e() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/litellm/config.yaml");
    let cfg = std::fs::read_to_string(path).expect("config litellm lisible");
    for attendu in [
        "model_name: mock-ok",
        "model_name: mock-missing-verdict",
        "model_name: mock-429",
        "model_name: mock-500",
        "mock_response: \"litellm.RateLimitError\"",
        "mock_response: \"litellm.InternalServerError\"",
        "master_key: os.environ/LITELLM_MASTER_KEY",
    ] {
        assert!(
            cfg.contains(attendu),
            "config.yaml doit contenir {attendu:?}"
        );
    }
}

fn env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("variable d'environnement {name} requise (job CI llm-litellm)"))
}

fn llm_cfg(model: &str, max_attempts: u32) -> LlmConfig {
    LlmConfig {
        base_url: env("EZ_TEST_LITELLM_BASE_URL"),
        api_key_env: "EZ_TEST_LITELLM_API_KEY".into(),
        model: model.into(),
        temperature: 0.2,
        top_p: 1.0,
        max_tokens: 4096,
        reasoning_effort: None,
        timeout_secs: 10,
        batch_size: 15,
        max_body_chars: 2000,
        rate_limit_per_min: 0,
        retry: RetryConfig {
            max_attempts,
            backoff_base_secs: 0,
        },
    }
}

fn req(model: &str) -> JsonChatRequest {
    JsonChatRequest {
        model: model.into(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: "PROMPT SYSTEME".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "batch de mails : uid=101, uid=102".into(),
            },
        ],
        temperature: 0.2,
        top_p: 1.0,
        max_tokens: 4096,
        response_format_json_schema: Some(BATCH_SCHEMA.into()),
    }
}

fn client(cfg: &LlmConfig, cle: &str) -> OpenAiLlmClient {
    OpenAiLlmClient::new(cfg, &|name| {
        assert_eq!(name, "EZ_TEST_LITELLM_API_KEY");
        Some(cle.to_string())
    })
    .unwrap()
}

fn cle_valide() -> String {
    env("EZ_TEST_LITELLM_API_KEY")
}

#[tokio::test]
#[ignore = "job CI llm-litellm uniquement : necessite le sidecar LiteLLM reel (EZ_TEST_LITELLM_*)"]
async fn mock_ok_livre_le_batch_complet_a_travers_le_proxy_reel() {
    let cfg = llm_cfg("mock-ok", 3);
    let client = client(&cfg, &cle_valide());

    let batch: Batch = client
        .chat_json_schema(req("mock-ok"), BATCH_SCHEMA)
        .await
        .unwrap();

    // bearer accepte par le proxy (sinon 4xx), format de reponse OpenAI-compatible,
    // contenu = la chaine mock_response fixe, deserialise en batch de verdicts
    assert_eq!(batch.verdicts.len(), 2);
    assert_eq!(batch.verdicts[0].uid, 101);
    assert_eq!(batch.verdicts[0].category, "Factures");
    assert_eq!(batch.verdicts[0].priority, 2);
    assert!(!batch.verdicts[0].is_newsletter);
    assert!((batch.verdicts[0].confidence - 0.97).abs() < 1e-6);
    assert_eq!(batch.verdicts[1].uid, 102);
    assert_eq!(batch.verdicts[1].category, "Archive");
    assert!((batch.verdicts[1].confidence - 0.9).abs() < 1e-6);
}

#[tokio::test]
#[ignore = "job CI llm-litellm uniquement : necessite le sidecar LiteLLM reel (EZ_TEST_LITELLM_*)"]
async fn mock_missing_verdict_reproduit_la_divergence_du_smoke() {
    // Smoke du jalon 8 (issue #21) : uid 101 recu en double, uid 102 omis. La
    // reponse devie du contrat attendu ; le transport (proxy reel) doit la livrer
    // telle quelle — la validation metier (warn + ecart) arrive avec #21.
    let cfg = llm_cfg("mock-missing-verdict", 3);
    let client = client(&cfg, &cle_valide());

    let batch: Batch = client
        .chat_json_schema(req("mock-missing-verdict"), BATCH_SCHEMA)
        .await
        .unwrap();

    assert_eq!(
        batch.verdicts.len(),
        2,
        "le proxy livre les 2 objets verdict tels quels : {batch:?}"
    );
    assert!(
        batch.verdicts.iter().all(|v| v.uid == 101),
        "uid 101 en double, uid 102 omis : {batch:?}"
    );
    assert_eq!(batch.verdicts[0].category, "Factures");
    assert_eq!(batch.verdicts[1].category, "Documents");
}

#[tokio::test]
#[ignore = "job CI llm-litellm uniquement : necessite le sidecar LiteLLM reel (EZ_TEST_LITELLM_*)"]
async fn modele_inconnu_retourne_400_non_retentif_depuis_le_proxy() {
    let cfg = llm_cfg("modele-inconnu", 3);
    let client = client(&cfg, &cle_valide());

    let err = client
        .chat_json_schema::<Batch>(req("modele-inconnu"), BATCH_SCHEMA)
        .await
        .unwrap_err();

    // 400 natif du proxy ("Invalid model name") : 4xx hors 429 = non retentif
    assert!(matches!(err, Error::LlmNonRetryable(_)), "err: {err}");
    assert!(err.to_string().contains("400"), "err: {err}");
}

#[tokio::test]
#[ignore = "job CI llm-litellm uniquement : necessite le sidecar LiteLLM reel (EZ_TEST_LITELLM_*)"]
async fn mock_429_du_proxy_est_retryable_puis_echoue() {
    // mock_response "litellm.RateLimitError" -> 429 natif du proxy (throttling_error)
    let cfg = llm_cfg("mock-429", 3);
    let client = client(&cfg, &cle_valide());

    let err = client
        .chat_json_schema::<Batch>(req("mock-429"), BATCH_SCHEMA)
        .await
        .unwrap_err();

    // 429 = retryable : 3 tentatives epuisees, erreur finale Error::Llm (pas NonRetryable)
    assert!(matches!(err, Error::Llm(_)), "err: {err}");
    assert!(err.to_string().contains("429"), "err: {err}");
}

#[tokio::test]
#[ignore = "job CI llm-litellm uniquement : necessite le sidecar LiteLLM reel (EZ_TEST_LITELLM_*)"]
async fn mock_500_du_proxy_est_retryable_puis_echoue() {
    let cfg = llm_cfg("mock-500", 3);
    let client = client(&cfg, &cle_valide());

    let err = client
        .chat_json_schema::<Batch>(req("mock-500"), BATCH_SCHEMA)
        .await
        .unwrap_err();

    assert!(matches!(err, Error::Llm(_)), "err: {err}");
    assert!(err.to_string().contains("500"), "err: {err}");
}

#[tokio::test]
#[ignore = "job CI llm-litellm uniquement : necessite le sidecar LiteLLM reel (EZ_TEST_LITELLM_*)"]
async fn mauvaise_cle_bearer_est_rejetee_par_le_proxy() {
    let cfg = llm_cfg("mock-ok", 3);
    let client = client(&cfg, "sk-mauvaise-cle-factice");

    let err = client
        .chat_json_schema::<Batch>(req("mock-ok"), BATCH_SCHEMA)
        .await
        .unwrap_err();

    // sans base de donnees, le proxy repond 400 (no_db_connection) a une cle
    // inconnue : 4xx hors 429, donc non retentif cote client
    assert!(matches!(err, Error::LlmNonRetryable(_)), "err: {err}");
    assert!(err.to_string().contains("400"), "err: {err}");
}
