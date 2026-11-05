use std::time::Duration;

use crate::config::RetryConfig;

/// Delai du tentative `attempt` (1-based) : backoff exponentiel `base * 2^(attempt-1)`,
/// borne a 5 minutes. Pure : testable sans dormir.
pub fn backoff_delay(cfg: &RetryConfig, attempt: u32) -> Duration {
    let base = cfg.backoff_base_secs;
    let shift = attempt.saturating_sub(1).min(10);
    let secs = base.saturating_mul(2u64.saturating_pow(shift)).min(300);
    Duration::from_secs(secs)
}

/// Execute `op` avec retry : `max_attempts` tentatives au total, backoff exponentiel
/// entre les tentatives. `sleep` est injecte (reel en prod, no-op en test) pour garder
/// le test rapide et deterministe. Toute erreur remonte apres la derniere tentative.
pub async fn with_retry<F, Fut, S, FutS, T>(
    cfg: &RetryConfig,
    mut op: F,
    mut sleep: S,
) -> crate::error::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = crate::error::Result<T>>,
    S: FnMut(Duration) -> FutS,
    FutS: std::future::Future<Output = ()>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                // erreur marquee comme definitive (ex: HTTP 4xx) : pas de retry
                if matches!(e, crate::error::Error::LlmNonRetryable(_)) {
                    return Err(e);
                }
                if attempt >= cfg.max_attempts.max(1) {
                    return Err(e);
                }
                let delay = backoff_delay(cfg, attempt);
                tracing::warn!(
                    attempt,
                    max_attempts = cfg.max_attempts,
                    delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "operation echouee, nouvelle tentative apres backoff"
                );
                sleep(delay).await;
            }
        }
    }
}

/// Sleep reel (tokio) pour la production.
pub fn sleep_tokio(d: Duration) -> impl std::future::Future<Output = ()> {
    tokio::time::sleep(d)
}
