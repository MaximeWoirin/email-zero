use email_zero::config::{Config, MailboxConfig};

const FULL_TOML: &str = r#"
[[mailbox]]
id = "orange"
host = "imap.orange.fr"
port = 993
username = "maxime@example.com"
password_env = "EZ_ORANGE_PASSWORD"
tls = true
poll_interval_secs = 300

[[mailbox]]
id = "proton"
host = "127.0.0.1"
port = 1143
username = "maxime@proton.me"
password_env = "EZ_PROTON_PASSWORD"
tls = false
poll_interval_secs = 600

[llm]
base_url = "https://litellm.example.com/v1"
api_key_env = "EZ_LLM_API_KEY"
model = "gpt-4o-mini"
temperature = 0.2
top_p = 1.0
max_tokens = 4096
reasoning_effort = "low"
timeout_secs = 90
batch_size = 15
rate_limit_per_min = 0

[llm.retry]
max_attempts = 3
backoff_base_secs = 5

[imap.retry]
max_attempts = 3
backoff_base_secs = 2

[triage]
action = "copy"
confidence_threshold = 0.8
prompt_path = "prompts/triage.md"

[discovery]
interval_secs = 86400
sample_per_folder = 20
examples_in_prompt = 3
exclude_folders = ["Sent", "Drafts", "Trash", "Junk"]

[log]
level = "info"
"#;

#[test]
fn parse_config_complete() {
    let cfg = Config::parse(FULL_TOML).expect("config valide");

    assert_eq!(cfg.mailboxes.len(), 2);
    let orange = &cfg.mailboxes[0];
    assert_eq!(orange.id, "orange");
    assert_eq!(orange.host, "imap.orange.fr");
    assert_eq!(orange.port, 993);
    assert_eq!(orange.username, "maxime@example.com");
    assert_eq!(orange.password_env.as_deref(), Some("EZ_ORANGE_PASSWORD"));
    assert!(orange.tls);
    assert_eq!(orange.poll_interval_secs, 300);

    assert_eq!(cfg.llm.model, "gpt-4o-mini");
    assert_eq!(cfg.llm.batch_size, 15);
    assert_eq!(cfg.llm.retry.max_attempts, 3);
    assert_eq!(cfg.imap_retry.retry.backoff_base_secs, 2);
    assert_eq!(cfg.triage.action, "copy");
    assert!((cfg.triage.confidence_threshold - 0.8).abs() < f32::EPSILON);
    assert_eq!(
        cfg.discovery.exclude_folders,
        vec!["Sent", "Drafts", "Trash", "Junk"]
    );
    assert_eq!(cfg.log.level, "info");
}

#[test]
fn valeurs_par_defaut_quand_sections_absentes() {
    let minimal = r#"
[[mailbox]]
id = "solo"
host = "127.0.0.1"
port = 1143
username = "a@b.c"
password_env = "EZ_PW"

[llm]
base_url = "http://localhost:4000/v1"
api_key_env = "EZ_LLM_API_KEY"
model = "local"
"#;
    let cfg = Config::parse(minimal).expect("config minimale valide");
    assert!(!cfg.mailboxes[0].tls, "tls vaut false par defaut");
    assert_eq!(cfg.triage.action, "copy");
    assert!((cfg.triage.confidence_threshold - 0.8).abs() < f32::EPSILON);
    assert_eq!(cfg.triage.prompt_path, "prompts/triage.md");
    assert_eq!(cfg.discovery.interval_secs, 86400);
    assert_eq!(cfg.discovery.sample_per_folder, 20);
    assert_eq!(cfg.discovery.examples_in_prompt, 3);
    assert_eq!(cfg.discovery.prompt_path, "prompts/discovery.md");
    assert_eq!(
        cfg.discovery.exclude_folders,
        vec!["Sent", "Drafts", "Trash", "Junk"]
    );
    assert_eq!(cfg.llm.batch_size, 15);
    assert_eq!(cfg.llm.timeout_secs, 90);
    assert_eq!(cfg.llm.retry.max_attempts, 3);
    assert_eq!(cfg.llm.retry.backoff_base_secs, 5);
    assert_eq!(cfg.imap_retry.retry.max_attempts, 3);
    assert_eq!(cfg.imap_retry.retry.backoff_base_secs, 2);
    assert_eq!(cfg.log.level, "info");
}

#[test]
fn ids_mailbox_uniques_sinon_erreur() {
    let dup = r#"
[[mailbox]]
id = "x"
host = "h"
port = 993
username = "u"
password_env = "P1"

[[mailbox]]
id = "x"
host = "h2"
port = 993
username = "u2"
password_env = "P2"

[llm]
base_url = "http://localhost/v1"
api_key_env = "K"
model = "m"
"#;
    let err = Config::parse(dup).unwrap_err();
    assert!(
        err.to_string().contains("x"),
        "l'erreur nomme l'id en double: {err}"
    );
}

#[test]
fn au_moins_une_mailbox_requise() {
    let empty = r#"
[llm]
base_url = "http://localhost/v1"
api_key_env = "K"
model = "m"
"#;
    assert!(Config::parse(empty).is_err());
}

#[test]
fn action_triage_valide_copy_ou_move() {
    let bad = FULL_TOML.replace("action = \"copy\"", "action = \"delete\"");
    let err = Config::parse(&bad).unwrap_err();
    assert!(
        err.to_string().contains("copy") || err.to_string().contains("move"),
        "{err}"
    );
}

#[test]
fn seuil_confiance_dans_0_1() {
    let bad = FULL_TOML.replace("confidence_threshold = 0.8", "confidence_threshold = 1.5");
    assert!(Config::parse(&bad).is_err());
}

#[test]
fn secret_password_env_resolu_via_lookup_injecte() {
    let mb = MailboxConfig {
        id: "t".into(),
        host: "h".into(),
        port: 993,
        username: "u".into(),
        password_env: Some("EZ_TEST_SECRET".into()),
        password_cmd: None,
        password_plain: None,
        tls: true,
        poll_interval_secs: 300,
    };
    let lookup = |name: &str| (name == "EZ_TEST_SECRET").then(|| "s3cret".to_string());
    assert_eq!(mb.resolve_password(&lookup).unwrap(), "s3cret");
}

#[test]
fn secret_manquant_erreur_nomme_la_variable() {
    let mb = MailboxConfig {
        id: "t".into(),
        host: "h".into(),
        port: 993,
        username: "u".into(),
        password_env: Some("EZ_TEST_ABSENT".into()),
        password_cmd: None,
        password_plain: None,
        tls: true,
        poll_interval_secs: 300,
    };
    let lookup = |_name: &str| None;
    let err = mb.resolve_password(&lookup).unwrap_err();
    assert!(err.to_string().contains("EZ_TEST_ABSENT"), "{err}");
}

#[test]
fn password_en_clair_tolere_mais_warning() {
    let mb = MailboxConfig {
        id: "t".into(),
        host: "h".into(),
        port: 993,
        username: "u".into(),
        password_env: None,
        password_cmd: None,
        password_plain: Some("en-clair".into()),
        tls: true,
        poll_interval_secs: 300,
    };
    assert!(mb.uses_plaintext_password());
    let lookup = |_name: &str| None;
    assert_eq!(mb.resolve_password(&lookup).unwrap(), "en-clair");
}

#[test]
fn store_path_par_defaut_quand_section_absente() {
    let minimal = r#"
[[mailbox]]
id = "solo"
host = "127.0.0.1"
port = 1143
username = "a@b.c"
password_env = "EZ_PW"

[llm]
base_url = "http://localhost:4000/v1"
api_key_env = "EZ_LLM_API_KEY"
model = "local"
"#;
    let cfg = Config::parse(minimal).expect("config minimale valide");
    assert_eq!(cfg.store.path, "data/email-zero.sqlite");
}

#[test]
fn store_path_explicite_dans_le_toml() {
    let toml_str = format!("{FULL_TOML}\n[store]\npath = \"/var/lib/email-zero/state.sqlite\"\n");
    let cfg = Config::parse(&toml_str).expect("config valide");
    assert_eq!(cfg.store.path, "/var/lib/email-zero/state.sqlite");
}

#[test]
fn max_body_chars_par_defaut_a_2000_quand_absent() {
    let minimal = r#"
[[mailbox]]
id = "solo"
host = "127.0.0.1"
port = 1143
username = "a@b.c"
password_env = "EZ_PW"

[llm]
base_url = "http://localhost:4000/v1"
api_key_env = "EZ_LLM_API_KEY"
model = "local"
"#;
    let cfg = Config::parse(minimal).expect("config minimale valide");
    assert_eq!(cfg.llm.max_body_chars, 2000);
}

#[test]
fn max_body_chars_explicite_dans_le_toml() {
    let toml_str = FULL_TOML.replacen(
        "rate_limit_per_min = 0",
        "rate_limit_per_min = 0\nmax_body_chars = 500",
        1,
    );
    let cfg = Config::parse(&toml_str).expect("config valide");
    assert_eq!(cfg.llm.max_body_chars, 500);
}

#[test]
fn imap_resilience_defaults_quand_section_imap_absente() {
    let minimal = r#"
[[mailbox]]
id = "solo"
host = "127.0.0.1"
port = 1143
username = "a@b.c"
password_env = "EZ_PW"

[llm]
base_url = "http://localhost:4000/v1"
api_key_env = "EZ_LLM_API_KEY"
model = "local"
"#;
    let cfg = Config::parse(minimal).expect("config minimale valide");
    assert_eq!(cfg.imap_retry.timeout_secs, 30);
    assert_eq!(cfg.imap_retry.keepalive_secs, 45);
    assert_eq!(cfg.imap_retry.max_reconnects, 3);
}

#[test]
fn imap_resilience_defaults_quand_seul_retry_est_fourni() {
    let toml_str = FULL_TOML.replacen(
        "[imap.retry]",
        "[imap]\ntimeout_secs = 10\n\n[imap.retry]",
        1,
    );
    let cfg = Config::parse(&toml_str).expect("config valide");
    assert_eq!(cfg.imap_retry.timeout_secs, 10);
    // champs non fournis : defaults (keepalive 45s, 3 reconnexions)
    assert_eq!(cfg.imap_retry.keepalive_secs, 45);
    assert_eq!(cfg.imap_retry.max_reconnects, 3);
}

#[test]
fn imap_resilience_valeurs_explicites_parsees() {
    let toml_str = FULL_TOML.replacen(
        "[imap.retry]",
        "[imap]\ntimeout_secs = 12\nkeepalive_secs = 0\nmax_reconnects = 5\n\n[imap.retry]",
        1,
    );
    let cfg = Config::parse(&toml_str).expect("config valide");
    assert_eq!(cfg.imap_retry.timeout_secs, 12);
    assert_eq!(cfg.imap_retry.keepalive_secs, 0, "0 = keepalive desactive");
    assert_eq!(cfg.imap_retry.max_reconnects, 5);
    // le retry existant n'est pas ecrase
    assert_eq!(cfg.imap_retry.retry.max_attempts, 3);
    assert_eq!(cfg.imap_retry.retry.backoff_base_secs, 2);
}
