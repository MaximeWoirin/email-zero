use clap::Parser;

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
async fn main() -> email_zero::error::Result<()> {
    let cli = Cli::parse();
    println!(
        "email-zero {} (config={}, dry_run={}, once={}, refresh_folders={})",
        env!("CARGO_PKG_VERSION"),
        cli.config,
        cli.dry_run,
        cli.once,
        cli.refresh_folders
    );
    Ok(())
}
