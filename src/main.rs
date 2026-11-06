use std::pin::Pin;

use clap::Parser;
use email_zero::clients::llm::OpenAiLlmClient;
use email_zero::config::Config;
use email_zero::error::Result;
use email_zero::folders::FolderCli;
use email_zero::runner::{daemon_loop, real_connect};
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

    /// Évalue et affiche les actions prévues sans rien écrire sur le serveur
    #[arg(long)]
    dry_run: bool,

    /// Une seule exécution puis sortie (manuel, cron, timer systemd)
    #[arg(long)]
    once: bool,

    /// Force le rafraîchissement des profils de dossiers
    #[arg(long)]
    refresh_folders: bool,

    /// Affiche l'état des dossiers du state store (source llm/user, whitelist,
    /// description, exemples) puis quitte, sans connexion IMAP ni LLM
    #[arg(long, conflicts_with_all = ["describe_folder", "reset_folder_description"])]
    list_folders: bool,

    /// Définit manuellement la description d'un dossier (texte libre, source
    /// 'user', jamais écrasée par la découverte) puis quitte
    #[arg(
        long,
        value_name = "DOSSIER",
        conflicts_with = "reset_folder_description"
    )]
    describe_folder: Option<String>,

    /// Texte de la nouvelle description pour --describe-folder (sinon lu sur
    /// stdin, multiligne jusqu'à EOF)
    #[arg(long, value_name = "TEXTE", requires = "describe_folder")]
    describe_text: Option<String>,

    /// Rend la main au profilage LLM pour la description d'un dossier (retour
    /// à la source 'llm') puis quitte
    #[arg(long, value_name = "DOSSIER")]
    reset_folder_description: Option<String>,

    /// Boîte cible des commandes de gestion des dossiers (sinon : toutes les
    /// boîtes configurées)
    #[arg(long, value_name = "ID")]
    mailbox: Option<String>,
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

    // Commandes de gestion des dossiers : elles agissent sur le state store
    // seul (pas de client LLM, pas de connexion IMAP) puis quittent.
    let folder_cli = FolderCli {
        list_folders: cli.list_folders,
        describe_folder: cli.describe_folder.clone(),
        describe_text: cli.describe_text.clone(),
        reset_folder_description: cli.reset_folder_description.clone(),
        mailbox: cli.mailbox.clone(),
    };
    if let Some(cmd) = folder_cli.command()? {
        let mut stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        return email_zero::folders::run_folder_command(
            &cfg,
            &mut store,
            &cmd,
            folder_cli.mailbox.as_deref(),
            &mut stdin,
            &mut stdout,
        );
    }

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

    // Boucle daemon : chaque iteration reconnecte chaque boite (une session
    // inactive est deconnectee par le serveur entre deux polls). --once = un
    // seul tour. Une boite en echec est logguee mais ne tue pas le daemon.
    let password_lookup = |name: &str| std::env::var(name).ok();
    let mut sleep = |secs: u64| {
        Ok(Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        })
            as Pin<Box<dyn std::future::Future<Output = ()> + Send>>)
    };
    let mut deps = email_zero::runner::DaemonDeps {
        connect_fn: &real_connect,
        password_lookup: &password_lookup,
        sleep: &mut sleep,
    };
    daemon_loop(
        &cfg,
        &llm,
        &mut store,
        cli.once,
        cli.dry_run,
        cli.refresh_folders,
        &mut deps,
    )
    .await
}
