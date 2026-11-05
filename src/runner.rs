use std::future::Future;
use std::pin::Pin;

use crate::clients::imap::{connect, BoxSession, ImapMailClient};
use crate::clients::llm::OpenAiLlmClient;
use crate::config::{Config, MailboxConfig};
use crate::discovery::{discovery_params_for, run_discovery};
use crate::error::Result;
use crate::pipeline::{pipeline_config_for, run, RunStats};
use crate::store::{parse_iso8601_utc, Store};

/// Connecteur IMAP injectable : reel (`connect`) en prod, fake en test.
/// La future est 'static : le connecteur recoit des donnees propres (clonees).
pub type ConnectFn =
    dyn Fn(MailboxConfig, String) -> Pin<Box<dyn Future<Output = Result<BoxSession>> + Send>>;

/// La decouverte des dossiers est due si la boite n'a jamais de run dans le
/// state store, ou si le dernier run date de plus de `interval_secs`
/// (timestamp de derniere execution dans le state store, spec design).
pub fn discovery_due(store: &Store, mailbox: &str, interval_secs: u64) -> Result<bool> {
    match store.last_discovery_run(mailbox)? {
        None => Ok(true),
        Some(run) => {
            let started = parse_iso8601_utc(&run.started_at).unwrap_or(0);
            Ok(crate::store::now_secs().saturating_sub(started) >= interval_secs)
        }
    }
}

/// Un poll sur une boite : connexion neuve (le daemon reconnecte a chaque
/// iteration — une session inactive pendant poll_interval_secs est deconnectee
/// par le serveur), job de decouverte des dossiers si `discover` (jalon 2c,
/// sur la meme session, avant le tri : la whitelist vient du store), puis
/// pipeline::run sur INBOX, log des stats.
pub async fn poll_mailbox(
    cfg: &Config,
    llm: &OpenAiLlmClient,
    store: &mut Store,
    mb: &MailboxConfig,
    opts: PollOptions,
    connect_fn: &ConnectFn,
    password_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<RunStats> {
    let password = mb.resolve_password(password_lookup)?;
    let session = connect_fn(mb.clone(), password).await?;
    let mut mail = ImapMailClient::new(session, cfg.imap_retry.retry.clone());
    if opts.discover {
        let dp = discovery_params_for(cfg)?;
        let dstats = run_discovery(&mut mail, llm, store, &mb.id, &dp).await?;
        tracing::info!(
            mailbox = %mb.id,
            folders_seen = dstats.folders_seen,
            whitelisted = dstats.whitelisted,
            sampled = dstats.sampled,
            profiled = dstats.profiled,
            status = dstats.status,
            "decouverte des dossiers terminee"
        );
    }
    let pc = pipeline_config_for(cfg, store, &mb.id, opts.dry_run)?;
    tracing::info!(
        mailbox = %mb.id,
        dry_run = pc.dry_run,
        whitelist = ?pc.whitelist,
        batch_size = pc.batch_size,
        max_body_chars = pc.max_body_chars,
        "pipeline config construite"
    );
    let stats = run(&mut mail, llm, store, &mb.id, "INBOX", &pc).await?;
    tracing::info!(
        mailbox = %mb.id,
        scanned = stats.scanned,
        pending = stats.pending,
        decided = stats.decided,
        copied = stats.copied,
        moved = stats.moved,
        planned = stats.planned,
        left_in_inbox = stats.left_in_inbox,
        "poll termine"
    );
    Ok(stats)
}

/// Flags d'un poll : `dry_run` (CLI --dry-run) et `discover` (job de decouverte
/// des dossiers due pour cette boite a ce tour).
#[derive(Debug, Clone, Copy, Default)]
pub struct PollOptions {
    pub dry_run: bool,
    pub discover: bool,
}

/// Parametres injectables de la boucle daemon (connecteur, lookup de secret,
/// sleep) regroupes pour garder une signature lisible.
pub struct DaemonDeps<'a> {
    pub connect_fn: &'a ConnectFn,
    pub password_lookup: &'a dyn Fn(&str) -> Option<String>,
    pub sleep: &'a mut dyn FnMut(u64) -> Result<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

/// Boucle daemon : itere sur les boites en reconnectant chacune a chaque tour.
/// Une boite en echec ne tue pas le daemon (log error, on continue). `once` =
/// un seul tour puis sortie ; sinon sommeil de min(poll_interval_secs) entre
/// deux tours. La decouverte des dossiers tourne quand elle est due
/// (`discovery.interval_secs` depuis le dernier run, ou `refresh_folders`).
pub async fn daemon_loop(
    cfg: &Config,
    llm: &OpenAiLlmClient,
    store: &mut Store,
    once: bool,
    dry_run: bool,
    refresh_folders: bool,
    deps: &mut DaemonDeps<'_>,
) -> Result<()> {
    let mut iteration = 0u64;
    loop {
        iteration += 1;
        for mb in &cfg.mailboxes {
            let discover =
                refresh_folders || discovery_due(store, &mb.id, cfg.discovery.interval_secs)?;
            match poll_mailbox(
                cfg,
                llm,
                store,
                mb,
                PollOptions { dry_run, discover },
                deps.connect_fn,
                deps.password_lookup,
            )
            .await
            {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(mailbox = %mb.id, iteration, error = %e, "poll echoue, on continue");
                }
            }
        }
        if once {
            break;
        }
        let interval_secs = cfg
            .mailboxes
            .iter()
            .map(|m| m.poll_interval_secs)
            .min()
            .unwrap_or(300);
        tracing::info!(iteration, interval_secs, "poll suivant");
        (deps.sleep)(interval_secs)?.await;
    }
    Ok(())
}

/// Connecteur reel.
pub fn real_connect(
    mb: MailboxConfig,
    password: String,
) -> Pin<Box<dyn Future<Output = Result<BoxSession>> + Send>> {
    Box::pin(async move { connect(&mb, &password).await })
}
