use clap::Parser;
use email_zero::config::Config;
use email_zero::error::Result;

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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::from_file(&cli.config)?;

    let plaintext = cfg.plaintext_password_mailboxes();
    if !plaintext.is_empty() {
        eprintln!(
            "WARNING: mot de passe en clair dans la config pour les boites: {} — \
             utiliser password_env ou password_cmd (tolere pour le POC uniquement)",
            plaintext.join(", ")
        );
    }

    println!(
        "email-zero {} (config={}, boites={}, dry_run={}, once={}, refresh_folders={})",
        env!("CARGO_PKG_VERSION"),
        cli.config,
        cfg.mailboxes.len(),
        cli.dry_run,
        cli.once,
        cli.refresh_folders
    );
    Ok(())
}
