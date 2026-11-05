use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use email_zero::clients::imap::BoxSession;
use email_zero::config::{Config, MailboxConfig};
use email_zero::error::{Error, Result};
use email_zero::runner::{daemon_loop, discovery_due, DaemonDeps};
use email_zero::store::Store;

fn test_config() -> Config {
    let toml = r#"
[[mailbox]]
id = "ok"
host = "127.0.0.1"
port = 1143
username = "a@b.c"
password_env = "EZ_TEST_PW"
poll_interval_secs = 60

[[mailbox]]
id = "ko"
host = "127.0.0.1"
port = 1143
username = "k@b.c"
password_env = "EZ_TEST_PW"
poll_interval_secs = 30

[llm]
base_url = "https://llm.example.com/v1"
api_key_env = "EZ_TEST_KEY"
model = "m"

[store]
path = ":memory:"
"#;
    Config::parse(toml).unwrap()
}

type ConnectFuture = Pin<Box<dyn std::future::Future<Output = Result<BoxSession>> + Send>>;
type Connect = Box<dyn Fn(MailboxConfig, String) -> ConnectFuture>;

fn make_connect(calls: Arc<AtomicUsize>, fail_ids: Vec<String>) -> Connect {
    Box::new(move |mb: MailboxConfig, _pw: String| {
        calls.fetch_add(1, Ordering::SeqCst);
        let fail = fail_ids.contains(&mb.id);
        Box::pin(async move {
            if fail {
                Err(Error::Imap(format!(
                    "connexion simulee en panne pour {}",
                    mb.id
                )))
            } else {
                Err(Error::Imap("pas de vraie session dans ce test".into()))
            }
        }) as ConnectFuture
    })
}

fn no_sleep() -> impl FnMut(u64) -> Result<Pin<Box<dyn std::future::Future<Output = ()> + Send>>> {
    |_secs| Ok(Box::pin(async {}) as Pin<Box<dyn std::future::Future<Output = ()> + Send>>)
}

#[tokio::test]
async fn daemon_once_ne_fait_qu_un_tour_mais_tente_toutes_les_boites() {
    let cfg = test_config();
    let calls = Arc::new(AtomicUsize::new(0));
    let connect = make_connect(calls.clone(), vec!["ko".into()]);

    let llm =
        email_zero::clients::llm::OpenAiLlmClient::new(&cfg.llm, &|_| Some("k".into())).unwrap();
    let mut store = Store::open_in_memory().unwrap();
    store
        .sync_mailboxes(&["ok".to_string(), "ko".to_string()])
        .unwrap();

    let pw = |_name: &str| Some("pw-test".to_string());
    let mut sleep = no_sleep();
    let mut deps = DaemonDeps {
        connect_fn: &*connect,
        password_lookup: &pw,
        sleep: &mut sleep,
    };
    daemon_loop(&cfg, &llm, &mut store, true, false, false, &mut deps)
        .await
        .unwrap();

    // un seul tour, mais les DEUX boites ont ete tentees ("ko" echoue, le daemon continue)
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn daemon_sans_once_reboucle_et_reconnecte_chaque_boite_a_chaque_tour() {
    let cfg = test_config();
    let calls = Arc::new(AtomicUsize::new(0));
    let connect = make_connect(calls.clone(), vec![]);

    let llm =
        email_zero::clients::llm::OpenAiLlmClient::new(&cfg.llm, &|_| Some("k".into())).unwrap();
    let mut store = Store::open_in_memory().unwrap();
    store
        .sync_mailboxes(&["ok".to_string(), "ko".to_string()])
        .unwrap();

    let sleeps = Arc::new(AtomicUsize::new(0));
    let sleeps_in = sleeps.clone();
    let pw = |_name: &str| Some("pw-test".to_string());
    let mut sleep = move |secs: u64| {
        assert_eq!(secs, 30, "intervalle = min des poll_interval_secs");
        let n = sleeps_in.fetch_add(1, Ordering::SeqCst);
        if n >= 2 {
            return Err(Error::Config("STOP_TEST_LOOP".into()));
        }
        Ok(Box::pin(async {}) as Pin<Box<dyn std::future::Future<Output = ()> + Send>>)
    };
    let mut deps = DaemonDeps {
        connect_fn: &*connect,
        password_lookup: &pw,
        sleep: &mut sleep,
    };
    let result = daemon_loop(&cfg, &llm, &mut store, false, false, false, &mut deps).await;

    // 3 tours effectues (le 3e stoppe dans le sleep), 2 boites par tour
    assert!(result.is_err(), "sortie par le stop du test");
    assert_eq!(sleeps.load(Ordering::SeqCst), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 3 * 2);
}

#[test]
fn discovery_due_sans_run_precedent_oui() {
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["ok".to_string()]).unwrap();
    assert!(discovery_due(&store, "ok", 86400).unwrap());
    // boite inconnue du store : due aussi
    assert!(discovery_due(&store, "jamais", 86400).unwrap());
}

#[test]
fn discovery_due_faux_avant_interval_vrai_apres() {
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["ok".to_string()]).unwrap();
    store.start_discovery_run("ok").unwrap();
    store.finish_discovery_run("ok", "ok", Some(2)).unwrap();
    // dernier run a l'instant : pas due avant l'intervalle
    assert!(!discovery_due(&store, "ok", 86400).unwrap());
    // intervalle nul : toujours due
    assert!(discovery_due(&store, "ok", 0).unwrap());
}

#[test]
fn discovery_due_reporte_quand_aucun_run_ok_ou_fallback() {
    // revue PR #13 : un run `error` (ou `running` orphelin) ne fait pas tourner
    // l'horloge — la decouverte reste due et est retentee au poll suivant.
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["ok".to_string()]).unwrap();
    store.start_discovery_run("ok").unwrap();
    store.finish_discovery_run("ok", "error", None).unwrap();
    assert!(
        discovery_due(&store, "ok", 86400).unwrap(),
        "dernier run = error, aucun ok/fallback : due (retente au poll suivant)"
    );
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["ok".to_string()]).unwrap();
    store.start_discovery_run("ok").unwrap(); // running orphelin (crash du process)
    assert!(discovery_due(&store, "ok", 86400).unwrap());
}

#[test]
fn discovery_due_un_error_apres_un_ok_recent_ne_redeclenche_pas() {
    // l'horloge tourne sur le dernier run ok/fallback : un error ulterieur ne
    // retente pas tant que l'intervalle n'est pas ecoule depuis le ok.
    let mut store = Store::open_in_memory().unwrap();
    store.sync_mailboxes(&["ok".to_string()]).unwrap();
    store.start_discovery_run("ok").unwrap();
    store.finish_discovery_run("ok", "ok", Some(2)).unwrap();
    store.start_discovery_run("ok").unwrap();
    store.finish_discovery_run("ok", "error", None).unwrap();
    assert!(
        !discovery_due(&store, "ok", 86400).unwrap(),
        "dernier ok/fallback date d'a l'instant : pas due malgre l'error suivant"
    );
}
