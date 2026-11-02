# Email Zero

Prétraitement d'emails par IA, headless, en Rust : un worker qui lit les nouveaux
mails en IMAP, les fait évaluer par un LLM (catégorie, criticité, résumé) et
écrit le tri directement dans les dossiers de la boîte. Le client mail habituel
reste l'interface unique — pas d'UI.

## Docs

- [Spec design v1](docs/superpowers/specs/2026-11-01-email-zero-design.md)
- [Plan d'implémentation — TDD](docs/superpowers/specs/2026-11-01-email-zero-plan.md)

## Statut

Jalon 0 livré : scaffold Rust (binaire `email-zero`, lib `config`/`ports`/`error`),
frontières IMAP/LLM en traits injectés, config TOML validée (secrets par
`*_env` uniquement), CI GitHub (fmt + clippy `-D warnings` + test).

Jalon 1 livré : state store SQLite (`src/store.rs`, schéma de la spec :
`mailbox`/`folder`/`processed`/`triage_result`/`discovery_run`) et logique de
décision pure (`src/triage.rs`) : whitelist string-match exacte côté Rust,
seuil de confiance (sous le seuil -> INBOX), action `copy`/`move`/`dry_run`,
gestion de l'`UIDVALIDITY` (changement = retraitement du dossier).
Méthodologie : TDD stricte, frontières IMAP/LLM en traits injectés.

Jalon 2a livré : moteur de pipeline de triage async (`src/pipeline.rs`), testé en
intégration avec fake IMAP + fake LLM scripté (zéro réseau, `tests/pipeline.rs`) :
`SEARCH UNSEEN` → dédup via le state store (changement d'`UIDVALIDITY` = retraitement
du dossier) → batchs LLM en structured output réassociés par `uid` → décision pure
(whitelist, seuil de confiance) → `copy`/`move` sur le serveur (aucune écriture en
`--dry-run`) → persistance `processed` + `triage_result`. Le prompt de triage v1 vit
dans `prompts/triage.md`, le chemin du state store dans `[store].path` du TOML.
Jalon 2b livré : clients réels IMAP (`async-imap` + rustls, `src/clients/imap.rs`) et LLM
OpenAI-compatible (`reqwest`, `src/clients/llm.rs`), retry/backoff exponentiel configurés
(`[imap.retry]`/`[llm.retry]`, `src/retry.rs`) ; `main` connecte chaque boîte et lance
`pipeline::run` sur INBOX (`--once` = un poll puis sortie, sinon poll toutes les
`poll_interval_secs`). Tests sans réseau externe : mini-serveurs IMAP/HTTP sur loopback.
Reste au jalon 2c : job de découverte des dossiers.

## Lancer

```sh
cp config.example.toml config.toml   # puis renseigner les noms de variables d'env
cargo run -- --config config.toml --dry-run --once
cargo test
```
