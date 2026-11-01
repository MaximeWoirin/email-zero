use email_zero::config::{Config, MailboxConfig};

fn mailbox(plain: Option<&str>, env: Option<&str>, cmd: Option<&str>) -> MailboxConfig {
    MailboxConfig {
        id: "t".into(),
        host: "h".into(),
        port: 993,
        username: "u".into(),
        password_env: env.map(str::to_string),
        password_cmd: cmd.map(str::to_string),
        password_plain: plain.map(str::to_string),
        tls: true,
        poll_interval_secs: 300,
    }
}

#[test]
fn debug_mailbox_config_ne_fuit_pas_le_secret_en_clair() {
    let mb = mailbox(Some("s3cret-en-clair-XYZ"), None, None);
    let dbg = format!("{mb:?}");
    assert!(
        !dbg.contains("s3cret-en-clair-XYZ"),
        "fuite dans Debug: {dbg}"
    );
    assert!(
        dbg.contains("***"),
        "le champ doit apparaitre masque: {dbg}"
    );
}

#[test]
fn debug_config_ne_fuit_pas_les_secrets() {
    let toml = r#"
[[mailbox]]
id = "orange"
host = "imap.orange.fr"
port = 993
username = "u"
password = "s3cret-config-ABC"

[llm]
base_url = "http://localhost/v1"
api_key_env = "EZ_LLM_API_KEY"
model = "m"
"#;
    let cfg = Config::parse(toml).unwrap();
    let dbg = format!("{cfg:?}");
    assert!(
        !dbg.contains("s3cret-config-ABC"),
        "fuite dans Debug: {dbg}"
    );
}

#[test]
fn resolve_password_via_env_injecte() {
    let mb = mailbox(None, Some("EZ_ANY"), None);
    let lookup = |name: &str| (name == "EZ_ANY").then(|| "valeur-secrete".to_string());
    assert_eq!(mb.resolve_password(&lookup).unwrap(), "valeur-secrete");
}

#[test]
fn resolve_password_env_manquant_erreur_nomme_la_variable() {
    let mb = mailbox(None, Some("EZ_ABSENT"), None);
    let lookup = |_name: &str| None;
    let err = mb.resolve_password(&lookup).unwrap_err();
    assert!(err.to_string().contains("EZ_ABSENT"), "{err}");
}

#[test]
fn resolve_password_cmd_execute_la_commande() {
    let mb = mailbox(None, None, Some("echo ez-cmd-secret-123"));
    let lookup = |_name: &str| None;
    let secret = mb.resolve_password(&lookup).unwrap();
    assert_eq!(secret, "ez-cmd-secret-123");
}

#[test]
fn resolve_password_cmd_echec_erreur() {
    let mb = mailbox(None, None, Some("exit 1"));
    let lookup = |_name: &str| None;
    assert!(mb.resolve_password(&lookup).is_err());
}

#[test]
fn plaintext_passwords_liste_les_mailboxes_concernees() {
    let toml = r#"
[[mailbox]]
id = "orange"
host = "h"
port = 993
username = "u"
password = "en-clair"

[[mailbox]]
id = "proton"
host = "h2"
port = 1143
username = "u2"
password_env = "EZ_PROTON_PASSWORD"

[llm]
base_url = "http://localhost/v1"
api_key_env = "K"
model = "m"
"#;
    let cfg = Config::parse(toml).unwrap();
    assert_eq!(cfg.plaintext_password_mailboxes(), vec!["orange"]);
}
