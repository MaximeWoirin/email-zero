//! Etage de conformite RFC 3501 (issue #17) : le vrai client IMAP
//! (`ImapMailClient`) contre un vrai serveur — Dovecot en conteneur, sur le
//! loopback uniquement, avec les fixtures maildir committes dans
//! `tests/fixtures/maildir/` (config `tests/dovecot/dovecot.conf`).
//!
//! Le scenario principal est `#[ignore]` : il n'est execute que par le job CI
//! `imap-dovecot` (`.github/workflows/ci.yml`), qui demarre Dovecot et renseigne
//! EZ_TEST_IMAP_HOST / EZ_TEST_IMAP_PORT / EZ_TEST_IMAP_USER / EZ_TEST_IMAP_PASSWORD.
//! `cargo test` standard le saute. Ce n'est ni le fake in-memory (tests pipeline
//! rapides) ni le mini-serveur (assertions sur les requetes emises) : c'est
//! l'arbitre de conformite qui attrape en CI les divergences fake/serveur reel
//! (ex. #16, BODY.PEEK[HEADER] -> BODY[HEADER]) avant le smoke reel du jalon 8.

use std::collections::HashMap;

use async_imap::types::Flag;
use email_zero::clients::imap::{connect, ImapMailClient};
use email_zero::config::{MailboxConfig, RetryConfig};
use email_zero::ports::MailClient;
use futures_util::TryStreamExt;

const SUJET_SIMPLE: &str = "Facture octobre";
const SUJET_MULTIPART: &str = "Rapport hebdomadaire";
const SUJET_UTF8: &str = "Résumé de l'été ☀";
const FROM_SIMPLE: &str = "Alice <alice@example.com>";
const FROM_UTF8: &str = "Équipe RH <rh@example.com>";

fn fixture_maildir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/maildir/testuser@example.test/Maildir")
}

/// Les fixtures committes doivent rester parsables (mail-parser) avec les
/// valeurs attendues par le scenario Dovecot : test rapide, zero reseau, tourne
/// dans le `cargo test` standard.
#[test]
fn fixtures_maildir_sont_parsables() {
    let dir = fixture_maildir();
    let raw_simple = std::fs::read(dir.join("new/msg-facture")).expect("fixture lisible");
    let simple = mail_parser::MessageParser::new()
        .parse(&raw_simple)
        .expect("fixture parsable RFC 5322");
    assert_eq!(simple.subject().unwrap(), SUJET_SIMPLE);
    assert_eq!(
        simple.from().unwrap().first().unwrap().address().unwrap(),
        "alice@example.com"
    );
    assert!(simple.body_text(0).unwrap().contains("facture EDF"));

    let raw_multipart = std::fs::read(dir.join("new/msg-rapport")).expect("fixture lisible");
    let multipart = mail_parser::MessageParser::new()
        .parse(&raw_multipart)
        .expect("fixture parsable RFC 5322");
    assert_eq!(multipart.subject().unwrap(), SUJET_MULTIPART);
    assert!(multipart.body_text(0).unwrap().contains("Version texte"));

    let raw_utf8 = std::fs::read(dir.join("cur/msg-resume-ete:2,S")).expect("fixture lisible");
    let utf8 = mail_parser::MessageParser::new()
        .parse(&raw_utf8)
        .expect("fixture parsable RFC 5322");
    assert_eq!(utf8.subject().unwrap(), SUJET_UTF8);
    let from = utf8.from().unwrap().first().unwrap();
    assert_eq!(from.name().unwrap(), "Équipe RH");
    assert_eq!(from.address().unwrap(), "rh@example.com");
    assert!(utf8.body_text(0).unwrap().contains("tout va bien"));
}

fn env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("variable d'environnement {name} requise (job CI imap-dovecot)"))
}

fn mailbox_config_from_env() -> MailboxConfig {
    MailboxConfig {
        id: "dovecot-test".into(),
        host: env("EZ_TEST_IMAP_HOST"),
        port: env("EZ_TEST_IMAP_PORT")
            .parse()
            .expect("EZ_TEST_IMAP_PORT doit etre un port u16"),
        username: env("EZ_TEST_IMAP_USER"),
        password_env: None,
        password_cmd: None,
        password_plain: Some(env("EZ_TEST_IMAP_PASSWORD")),
        tls: false,
        poll_interval_secs: 300,
    }
}

async fn connect_client() -> ImapMailClient {
    let mb = mailbox_config_from_env();
    let password = mb.resolve_password_from_env().unwrap();
    let session = connect(&mb, &password)
        .await
        .expect("connexion + LOGIN sur le serveur Dovecot reel");
    ImapMailClient::new(
        session,
        RetryConfig {
            max_attempts: 1,
            backoff_base_secs: 0,
        },
    )
}

async fn sujets(client: &mut ImapMailClient, folder: &str, uids: &[u32]) -> Vec<String> {
    let mut out = Vec::with_capacity(uids.len());
    for uid in uids {
        out.push(client.fetch_headers(folder, *uid).await.unwrap().subject);
    }
    out
}

#[tokio::test]
#[ignore = "job CI imap-dovecot uniquement : necessite un serveur Dovecot reel (EZ_TEST_IMAP_*)"]
async fn client_reel_dovecot_conformite_rfc3501() {
    let mut client = connect_client().await;

    // LIST : INBOX + dossiers special-use des fixtures (.Sent/.Archive/.Trash)
    let folders = client.list_folders().await.unwrap();
    let by_name: HashMap<&str, Option<&str>> = folders
        .iter()
        .map(|f| (f.name.as_str(), f.special_use.as_deref()))
        .collect();
    assert!(
        by_name.contains_key("INBOX"),
        "LIST doit contenir INBOX : {by_name:?}"
    );
    assert_eq!(by_name.get("Sent"), Some(&Some("\\Sent")), "{by_name:?}");
    assert_eq!(
        by_name.get("Archive"),
        Some(&Some("\\Archive")),
        "{by_name:?}"
    );
    assert_eq!(by_name.get("Trash"), Some(&Some("\\Trash")), "{by_name:?}");

    // UIDVALIDITY : porte par la reponse SELECT, > 0 et stable
    let uv = client.uidvalidity("INBOX").await.unwrap();
    assert!(uv > 0, "UIDVALIDITY doit etre > 0");
    assert_eq!(client.uidvalidity("INBOX").await.unwrap(), uv);

    // SEARCH ALL / UNSEEN : 3 messages en fixture, 2 non lus (new/), 1 lu (cur/:2,S)
    let all = client.search_all("INBOX").await.unwrap();
    assert_eq!(all.len(), 3, "3 messages en fixture INBOX : {all:?}");
    let unseen = client.search_unseen("INBOX").await.unwrap();
    assert_eq!(
        unseen.len(),
        2,
        "2 messages non lus en fixture : {unseen:?}"
    );

    // Identification des UIDs par sujet via fetch_headers : BODY.PEEK[HEADER]
    // contre un vrai serveur doit produire BODY[HEADER] (aurait attrape #16)
    let mut uid_par_sujet: HashMap<String, u32> = HashMap::new();
    for uid in &all {
        let h = client.fetch_headers("INBOX", *uid).await.unwrap();
        uid_par_sujet.insert(h.subject.clone(), *uid);
    }
    let uid_simple = *uid_par_sujet
        .get(SUJET_SIMPLE)
        .expect("fixture simple presente");
    let uid_multipart = *uid_par_sujet
        .get(SUJET_MULTIPART)
        .expect("fixture multipart presente");
    let uid_utf8 = *uid_par_sujet
        .get(SUJET_UTF8)
        .expect("fixture utf-8 presente");
    assert!(unseen.contains(&uid_simple) && unseen.contains(&uid_multipart));
    assert!(
        !unseen.contains(&uid_utf8),
        "le message lu (cur/:2,S) ne doit pas etre UNSEEN"
    );

    // fetch_headers : en-tete exact du message simple
    let h = client.fetch_headers("INBOX", uid_simple).await.unwrap();
    assert_eq!(h.subject, SUJET_SIMPLE);
    assert_eq!(h.from, FROM_SIMPLE);
    assert!(
        h.date.starts_with("2026-11-01T10:00:00"),
        "date : {}",
        h.date
    );

    // fetch_body : parsing RFC 5322 des trois fixtures (simple, multipart, non-ASCII)
    let mail = client.fetch_body("INBOX", uid_simple).await.unwrap();
    assert_eq!(mail.header.subject, SUJET_SIMPLE);
    assert!(
        mail.body_text.contains("facture EDF"),
        "body : {}",
        mail.body_text
    );

    let mail_mp = client.fetch_body("INBOX", uid_multipart).await.unwrap();
    assert!(
        mail_mp.body_text.contains("Version texte"),
        "multipart text/plain attendu : {}",
        mail_mp.body_text
    );

    let mail_utf8 = client.fetch_body("INBOX", uid_utf8).await.unwrap();
    assert_eq!(mail_utf8.header.subject, SUJET_UTF8);
    assert_eq!(mail_utf8.header.from, FROM_UTF8);
    assert!(
        mail_utf8.body_text.contains("tout va bien"),
        "quoted-printable decode attendu : {}",
        mail_utf8.body_text
    );

    // fetch_headers + fetch_body ne posent jamais \Seen (BODY.PEEK) :
    // 1) le message reste UNSEEN cote trait
    assert!(client
        .search_unseen("INBOX")
        .await
        .unwrap()
        .contains(&uid_simple));
    // 2) verification via FLAGS sur une session brute
    let mb = mailbox_config_from_env();
    let password = mb.resolve_password_from_env().unwrap();
    let mut session = connect(&mb, &password).await.unwrap();
    session.select("INBOX").await.unwrap();
    let stream = session
        .uid_fetch(uid_simple.to_string(), "FLAGS")
        .await
        .unwrap();
    let fetches: Vec<_> = stream.try_collect().await.unwrap();
    let flags: Vec<Flag<'_>> = fetches
        .iter()
        .find(|f| f.uid == Some(uid_simple))
        .expect("reponse FETCH FLAGS pour l'uid")
        .flags()
        .collect();
    assert!(
        !flags.contains(&Flag::Seen),
        "BODY.PEEK ne doit pas poser \\Seen : {flags:?}"
    );

    // COPY : la source reste, la cible recoit le message
    let inbox_avant = client.search_all("INBOX").await.unwrap().len();
    client.copy("INBOX", uid_simple, "Archive").await.unwrap();
    assert_eq!(
        client.search_all("INBOX").await.unwrap().len(),
        inbox_avant,
        "COPY ne retire pas le message de la source"
    );
    let arch = client.search_all("Archive").await.unwrap();
    let sujets_arch = sujets(&mut client, "Archive", &arch).await;
    assert!(
        sujets_arch.iter().any(|s| s == SUJET_SIMPLE),
        "COPY doit livrer le message dans Archive : {sujets_arch:?}"
    );

    // MOVE : la source perd le message, la cible le recoit
    client
        .r#move("INBOX", uid_multipart, "Archive")
        .await
        .unwrap();
    let inbox_apres = client.search_all("INBOX").await.unwrap();
    assert_eq!(
        inbox_apres.len(),
        inbox_avant - 1,
        "MOVE retire le message de la source"
    );
    assert!(!inbox_apres.contains(&uid_multipart));
    let arch2 = client.search_all("Archive").await.unwrap();
    let sujets_arch2 = sujets(&mut client, "Archive", &arch2).await;
    assert!(
        sujets_arch2.iter().any(|s| s == SUJET_MULTIPART),
        "MOVE doit livrer le message dans Archive : {sujets_arch2:?}"
    );

    // UIDVALIDITY toujours stable apres les operations
    assert_eq!(client.uidvalidity("INBOX").await.unwrap(), uv);
}
