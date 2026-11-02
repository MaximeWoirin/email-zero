# Email Zero — Plan d'implémentation

*Basé sur le design "Email Zero — Design v1" (validé). Méthodologie : TDD stricte.*

## Écarts actés (jalon 0, 2026-11-01)

- **Crate unique** au lieu d'un workspace à 6 crates (ez-config, ez-store…). Pour un POC
  c'est mieux : moins de cérémonie, compilation plus rapide. Écart endossé.
- **Pas de `rustfmt.toml` / `clippy.toml`** : les defaults suffisent. Non bloquant.
- **Sécurité des secrets (corrigé en jalon 0+)** : `Debug` custom sur `MailboxConfig`
  masque les champs secret (`***`) ; test de rédaction `tests/secrets.rs`. La lecture
  d'env est **injectée** dans `resolve_password(&dyn Fn(&str) -> Option<String>)` :
  aucun `std::env::set_var`/`unsafe` dans les tests (ils tournaient en parallèle dans le
  même process — racy). `password_cmd` implémenté (`sh -c`, sortie stdout trimmée) et le
  warning « mot de passe en clair » est branché au chargement de la config dans `main`.

## Règles du jeu (transverses)

**Le cycle TDD est obligatoire pour chaque brique** : un test qui échoue → constat de l'échec → code minimal pour le passer → suite verte → refactor. Aucune ligne de code de production n'est écrite avant son test.

**Frontières en traits injectés** — c'est la clé pour mocker sans douleur :

```rust
/// Toutes les opérations IMAP passent par ce trait.
/// Vrai client (async-imap) en prod, fake in-memory en test.
pub trait MailClient {
    async fn list_folders(&mut self) -> Result<Vec<Folder>>;
    async fn search_unseen(&mut self, folder: &str) -> Result<Vec<u32>>;
    async fn fetch_headers(&mut self, folder: &str, uid: u32) -> Result<MailHeader>;
    async fn fetch_body(&mut self, folder: &str, uid: u32) -> Result<ParsedMail>;
    async fn copy(&mut self, folder: &str, uid: u32, target: &str) -> Result<()>;
    async fn r#move(&mut self, folder: &str, uid: u32, target: &str) -> Result<()>;
}

/// Toute requête LLM passe par ce trait.
/// Vrai client (async-openai) en prod, fake scripté en test.
pub trait LlmClient {
    async fn chat_json<T: DeserializeOwned>(&self, req: JsonChatRequest) -> Result<T>;
    async fn chat_json_schema<T: DeserializeOwned>(&self, req: JsonChatRequest, schema: &str) -> Result<T>;
}
```

**Trois niveaux de tests** :
1. **Unitaires** (rapides, par fonction) : parsing config, logique UIDVALIDITY, validation whitelist, seuil de confiance.
2. **Intégration** (le cœur, ce qu'on cherche) : le pipeline complet — fake IMAP rempli de mails, fake LLM avec des réponses scriptées, assertions sur les actions produites et le contenu du state store. Zéro réseau.
3. **Smoke réel** (hors CI, manuel) : `--dry-run` contre les vraies boîtes Orange et Proton. Jamais d'écriture en CI, jamais de secret dans les tests.

**Chaque test est nommé d'après une décision de la spec** : `test_erreurs`, `confidence_sous_seuil_reste_dans_inbox`, `uidvalidity_change_retraite_le_dossier`, etc. La spec est la source des tests.

**CI dès le jour 1** : `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`. Aucun merge avec un warning.

## Traçabilité jalons ↔ PR

| Jalon | Contenu | PR | Statut |
|-------|---------|-----|--------|
| 0 | Scaffold Rust, config TOML validée, frontières en traits, CI | #1 (branche `vibe/jalon-0-scaffold-ci-ad3e52`) | mergée |
| 0+ | Durcissement secrets (Debug masqué, env injecté, `password_cmd`, warning clair) + écarts actés | #3 (mergée dans la branche du jalon 0, **pas dans main** — à merger séparément) | mergée |
| 1 | State store SQLite (schéma spec) + décision de triage pure (whitelist, seuil, UIDVALIDITY) | #4 (`vibe/jalon-1-store-triage-f47681`) | mergée |
| 2a | Moteur de pipeline async (`src/pipeline.rs`) testé avec fake IMAP + fake LLM (tests d'intégration zéro réseau) : `SEARCH UNSEEN` → dédup `processed` (UIDVALIDITY exposé par `MailClient::uidvalidity`) → batchs LLM structured output réassociés par `uid` → décision pure → `copy`/`move` (rien en `--dry-run`) → persistance `processed` + `triage_result`. Prompt de triage v1 (`prompts/triage.md`), schéma batch `BATCH_SCHEMA`. `store.path` dans le TOML, logging JSON (`main`), bornage du corps `[llm] max_body_chars` (défaut 2000, issue #7), flag CLI `--dry-run` câblé jusqu'au `PipelineConfig` (issue #8, paramètre explicite). Clients réels + daemon = 2b/2c | #6 (`vibe/jalon-2a-pipeline-triage-6fdc40`) + branche issue #8 | mergée |
| 2b | Clients réels IMAP (`async-imap` + rustls, `src/clients/imap.rs`) et LLM OpenAI-compatible (`reqwest`, `src/clients/llm.rs`), retry/backoff exponentiel configurés (`src/retry.rs`, `[imap.retry]`/`[llm.retry]`) ; `main` câble connexion réelle + `pipeline::run` en `--once`/poll. Tests sans réseau externe : mini-serveurs IMAP/HTTP sur loopback (`tests/clients_imap.rs`, `tests/clients_llm.rs`, `tests/end_to_end_2b.rs`) | #10 (`vibe/jalon-2b-clients-reels-6fdc40`) | ouvert |
| 2c | Job de découverte des dossiers (LIST + special-use, échantillonnage, profilage LLM, `discovery_run`) + daemon `--once`/poll par boîte | — | à venir |

Notes pour plus tard (actées en revue de #4) :

- `Store::pending_uids` fait un COUNT par UID (N+1) : OK à l'échelle d'une boîte perso, à rebattre en un seul `IN (...)` si le backfill historique (v2, API Batch) arrive.
- `LlmVerdict` ne porte pas le `uid` : la réassociation verdict → mail se fera dans le moteur de triage (le batch embarque les uid dans le prompt).
- Le chemin du fichier SQLite est dans le TOML (`[store].path`, défaut `data/email-zero.sqlite`) depuis le jalon 2a.

