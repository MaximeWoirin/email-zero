use email_zero::config::Config;

#[test]
fn config_example_du_repo_est_valide() {
    let content = include_str!("../config.example.toml");
    let cfg = Config::parse(content).expect("config.example.toml doit parser et valider");
    assert_eq!(cfg.mailboxes.len(), 2);
    assert!(cfg.mailboxes.iter().all(|m| m.password_env.is_some()));
    assert!(cfg.mailboxes.iter().all(|m| !m.uses_plaintext_password()));
}
