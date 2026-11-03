use std::cell::RefCell;
use std::time::Duration;

use email_zero::config::RetryConfig;
use email_zero::error::Error;
use email_zero::retry::{backoff_delay, with_retry};

fn cfg(max_attempts: u32, backoff_base_secs: u64) -> RetryConfig {
    RetryConfig {
        max_attempts,
        backoff_base_secs,
    }
}

#[test]
fn backoff_exponentiel_borne_a_5_minutes() {
    let c = cfg(10, 5);
    assert_eq!(backoff_delay(&c, 1), Duration::from_secs(5));
    assert_eq!(backoff_delay(&c, 2), Duration::from_secs(10));
    assert_eq!(backoff_delay(&c, 3), Duration::from_secs(20));
    assert_eq!(backoff_delay(&c, 4), Duration::from_secs(40));
    assert_eq!(backoff_delay(&c, 100), Duration::from_secs(300));
}

#[test]
fn backoff_base_zero_reste_zero() {
    let c = cfg(3, 0);
    assert_eq!(backoff_delay(&c, 1), Duration::ZERO);
    assert_eq!(backoff_delay(&c, 5), Duration::ZERO);
}

#[tokio::test]
async fn succes_au_premier_essai_ne_reessaie_pas() {
    let calls = RefCell::new(0u32);
    let r = with_retry(
        &cfg(3, 5),
        || {
            *calls.borrow_mut() += 1;
            async { Ok::<_, Error>(42) }
        },
        |_| async {},
    )
    .await
    .unwrap();
    assert_eq!(r, 42);
    assert_eq!(*calls.borrow(), 1);
}

#[tokio::test]
async fn echecs_puis_succes_reessaie_avec_backoff() {
    let calls = RefCell::new(0u32);
    let delays = RefCell::new(Vec::new());
    let r = with_retry(
        &cfg(3, 5),
        || {
            let n = {
                let mut c = calls.borrow_mut();
                *c += 1;
                *c
            };
            async move {
                if n < 3 {
                    Err(Error::Imap(format!("echec {n}")))
                } else {
                    Ok("ok")
                }
            }
        },
        |d| {
            delays.borrow_mut().push(d);
            async {}
        },
    )
    .await
    .unwrap();
    assert_eq!(r, "ok");
    assert_eq!(*calls.borrow(), 3);
    assert_eq!(
        *delays.borrow(),
        vec![Duration::from_secs(5), Duration::from_secs(10)]
    );
}

#[tokio::test]
async fn erreur_definitive_apres_max_attempts() {
    let calls = RefCell::new(0u32);
    let err = with_retry(
        &cfg(3, 2),
        || {
            *calls.borrow_mut() += 1;
            async { Err::<(), _>(Error::Llm("toujours en panne".into())) }
        },
        |_| async {},
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("toujours en panne"));
    assert_eq!(*calls.borrow(), 3);
}

#[tokio::test]
async fn erreur_non_retryable_court_circuite_le_retry() {
    let calls = RefCell::new(0u32);
    let err = with_retry(
        &cfg(5, 0),
        || {
            *calls.borrow_mut() += 1;
            async { Err::<(), _>(Error::LlmNonRetryable("400 bad request".into())) }
        },
        |_| async {},
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::LlmNonRetryable(_)));
    assert_eq!(
        *calls.borrow(),
        1,
        "une erreur non-retryable ne doit pas etre retentee"
    );
}

#[tokio::test]
async fn max_attempts_zero_est_traite_comme_une_seule_tentative() {
    let calls = RefCell::new(0u32);
    let err = with_retry(
        &cfg(0, 2),
        || {
            *calls.borrow_mut() += 1;
            async { Err::<(), _>(Error::Imap("nope".into())) }
        },
        |_| async {},
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("nope"));
    assert_eq!(*calls.borrow(), 1);
}
