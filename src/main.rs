use clap::Parser;
use email_zero::clients::imap::{connect, ImapMailClient};
use email_zero::clients::llm::OpenAiLlmClient;
use email_zero::config::Config;
use email_zero::error::Result;
use email_zero::pipeline::{pipeline_config_for, run};
use email_zero::store::Store;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "email-zero",
    version,
    about = "Pre-traitement d'emails par IA (worker headless IMAP + LLM)"
)]
struct Cli {
    /// Chemin du fichier de config TOML
    #[arg(long, default_value = "config.toml")]
    config: String,

    /// Evalue et affiche les actions prevues sans rien ecrire sur le serveur
    #[arg(long)]
    dry_run: bool,

    /// Une seule execution puis sortie (manuel, cron, timer systemd)
    #[arg(long)]
    once: bool,

    /// Force le rafraichissement des profils de dossiers
    #[arg(long)]
    refresh_folders: bool,
}

fn init_logging(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::from_file(&cli.config)?;
    init_logging(&cfg.log.level);

    let plaintext = cfg.plaintext_password_mailboxes();
    if !plaintext.is_empty() {
        tracing::warn!(
            mailboxes = ?plaintext,
            "mot de passe en clair dans la config — utiliser password_env ou password_cmd \
             (tolere pour le POC uniquement)"
        );
    }

    let mut store = Store::open(&cfg.store.path)?;
    let ids: Vec<String> = cfg.mailboxes.iter().map(|m| m.id.clone()).collect();
    store.sync_mailboxes(&ids)?;

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %cli.config,
        store_path = %cfg.store.path,
        mailboxes = ?ids,
        dry_run = cli.dry_run,
        once = cli.once,
        refresh_folders = cli.refresh_folders,
        "email-zero demarre"
    );

    let llm = OpenAiLlmClient::new_from_env(&cfg.llm)?;

    // Fail-fast assume (revue PR #9) : pipeline_config_for lit triage.prompt_path,
    // donc le binaire refuse de demarrer si le fichier de prompt est absent.
    for mb in &cfg.mailboxes {
        let password = mb.resolve_password_from_env()?;
        let session = connect(mb, &password).await?;
        let mut mail = ImapMailClient::new(session, cfg.imap_retry.retry.clone());
        let pc = pipeline_config_for(&cfg, &store, &mb.id, cli.dry_run)?;
        tracing::info!(
            mailbox = %mb.id,
            dry_run = pc.dry_run,
            whitelist = ?pc.whitelist,
            batch_size = pc.batch_size,
            max_body_chars = pc.max_body_chars,
            "pipeline config construite"
        );
        let stats = run(&mut mail, &llm, &mut store, &mb.id, "INBOX", &pc).await?;
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
        if !cli.once {
            let interval = tokio::time::Duration::from_secs(mb.poll_interval_secs);
            tracing::info!(mailbox = %mb.id, interval_secs = mb.poll_interval_secs, "poll suivant");
            tokio::time::sleep(interval).await;
        }
    }
    Ok(())
}
