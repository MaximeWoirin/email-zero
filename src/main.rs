use clap::Parser;
use email_zero::config::Config;
use email_zero::error::Result;
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
        eprintln!(
            "WARNING: mot de passe en clair dans la config pour les boites: {} — \
             utiliser password_env ou password_cmd (tolere pour le POC uniquement)",
            plaintext.join(", ")
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

    if cli.once || cli.dry_run || cli.refresh_folders {
        tracing::info!(
            "les clients IMAP/LLM reels et le job de decouverte arrivent au jalon 2b ; \
             aucun acces serveur effectue (state store ouvert et synchronise)"
        );
    }
    Ok(())
}
