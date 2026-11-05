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
| 2a | Moteur de pipeline async (`src/pipeline.rs`) testé avec fake IMAP + fake LLM (tests d'intégration zéro réseau) : `SEARCH UNSEEN` → dédup `processed` (UIDVALIDITY exposé par `MailClient::uidvalidity`) → batchs LLM structured output réassociés par `uid` → décision pure → `copy`/`move` (rien en `--dry-run`) → persistance `processed` + `triage_result`. Prompt de triage v1 (`prompts/triage.md`), schéma batch `BATCH_SCHEMA`. `store.path` dans le TOML, logging JSON (`main`), bornage du corps `[llm] max_body_chars` (défaut 2000, issue #7), flag CLI `--dry-run` câblé jusqu'au `PipelineConfig` (issue #8, paramètre explicite). Clients réels + daemon = 2b/2c | #6 (`vibe/jalon-2a-pipeline-triage-6fdc40`) + #9 (`vibe/issue-8-dry-run-cable-6fdc40`) | mergée |
| 2b | Clients réels IMAP (`async-imap` + rustls, `src/clients/imap.rs`, fetch en `BODY.PEEK[]` pour ne jamais poser `\Seen`, UIDVALIDITY lue sur la réponse SELECT sans STATUS) et LLM OpenAI-compatible (`reqwest`, `src/clients/llm.rs`, base_url https exigée hors loopback), retry/backoff (`src/retry.rs`), daemon en boucle avec reconnexion par itération (`main`) ; tests loopback uniquement | #10 (`vibe/jalon-2b-clients-reels-6fdc40`) | ouvert |
| 2c | Job de découverte des dossiers (`src/discovery.rs` : LIST + special-use RFC 6154, INBOX hors whitelist, `exclude_folders` par nom ou special-use, échantillonnage des N derniers headers `SEARCH ALL` + `BODY.PEEK[HEADER]`, **une seule requête LLM** de profilage (`PROFILING_SCHEMA`, réassociation par `name`, prompt `prompts/discovery.md`), profils en cache `folder` avec `updated_at`, `discovery_run` ok/fallback/error, fallback = échantillons bruts sans description) câblé dans le daemon (due si aucun run ou `discovery.interval_secs` écoulé depuis le dernier run, `--refresh-folders` force) + `MailClient::search_all` + `parse_iso8601_utc` + `examples_in_prompt` bornant les exemples dans le prompt de triage | #13 (`vibe/jalon-2c-discovery-dossiers-c0cb9f`) | ouvert |

## Plan détaillé par jalon (référence TDD)

*Plan initial conservé comme référence des tests de chaque brique. L'exécution réelle a regroupé et découpé différemment : le jalon 2 ci-dessous correspond aux jalons 2a/2b/2c du tableau de traçabilité (découpe actée en revue de #4) ; le tableau fait foi pour l'avancement.*

## Jalon 0 — Scaffold + CI

- `cargo init` avec workspace, `clippy.toml`, `rustfmt`.
- GitHub Actions (ou équivalent) : fmt + clippy + test sur push.
- Structure de crates : `ez-config`, `ez-store`, `ez-mail`, `ez-llm`, `ez-triage`, binaire `emailzero`.

*Test du jalon : le CI tourne vert sur un projet vide. Rien d'autre.*

## Jalon 1 — Config (`ez-config`)

Tests d'abord (unitaires) :
- un TOML minimal parse 1 boîte avec defaults partout ;
- tous les defaults documentés dans la spec sont respectés (`poll_interval_secs = 300`, `temperature = 0.2`, `batch_size = 15`, `confidence_threshold = 0.8`, `action = "copy"`, `discovery.interval_secs = 86400`…) ;
- un TOML invalide produit une erreur claire, ligne et champ ;
- résolution des secrets : `password_env` lu depuis l'env, `password_cmd` exécuté — jamais de mot de passe en clair accepté sans warning ;
- mécanisme : enum `SecretRef` (serde untagged) — la présence du champ (`password_env` / `password_cmd` / `password`) décide de la variante ; aucun des trois = erreur claire nommant la boîte ; résolution en deux temps (parse → `resolve()` au démarrage, env absente ou commande en échec = refus de démarrer la boîte) ;
- `Debug`/`Display` de `SecretRef` rédactés (`***`) : le log de la config chargée ne contient jamais le secret résolu (testé explicitement) ;
- `[[mailbox]]` multiples : ids uniques requis, doublon = erreur ;
- les paramètres modèle (`temperature`, `top_p`, `max_tokens`, `reasoning_effort`) sont optionnels et transmis tels quels.

Vert : `Config::load()` + `Config` typé avec defaults.

## Jalon 2 — State store (`ez-store`)

Tests d'abord :
- les migrations créent les 5 tables de la spec, idempotent ;
- un UID marqué traité n'est pas retourné comme non traité au poll suivant ;
- changement de `UIDVALIDITY` → les UIDs traités de l'ancienne validity ne comptent plus (retraitement) ;
- insertion d'un `triage_result` avec FK vers `processed` : un résultat sans UID traité correspondant est refusé ;
- profil de dossier : save + reload, `excluded` filtré de la whitelist ;
- `discovery_run` : statut `ok` | `fallback` | `error`.

Vert : `Store` sur rusqlite + migrations embarquées. DB en fichier (prod) ou en mémoire (tests).

## Jalon 3 — Fetcher IMAP (`ez-mail`)

Tests d'abord (**intégration avec fake `MailClient`**) :
- `search_unseen` retourne les UID non lus ; les UIDs déjà dans `processed` sont exclus — c'est la règle "non traité, pas nouveau" ;
- un mail déjà traité mais marqué non lu n'est pas retraité (le flag SEEN n'est jamais modifié — testé explicitement : aucune action `store`/`seen` sur les flags dans ce chemin) ;
- le fetcher limite le lot à `batch_size` ;
- un dossier INBOX vide → lot vide, aucune requête LLM émise.

Vert : `Fetcher` branché sur le trait `MailClient`.

## Jalon 4 — Découverte des dossiers (`ez-mail` + `ez-llm`)

Tests d'abord :
- parsing de la réponse `LIST` : dossiers avec special-use `Sent`, `Drafts`, `Trash`, `Junk` exclus, `Archive` conservé, `\Noselect` exclu ;
- `exclude_folders` de la config s'ajoute aux exclusions RFC 6154 ;
- échantillonnage : les N derniers headers par dossier alimentent le prompt de profilage ;
- **une seule requête LLM** pour tous les dossiers d'une boîte (assertion sur le nombre d'appels au fake) ;
- réponse du fake LLM → profils sauvegardés dans `folder` avec `updated_at` ;
- LLM indisponible au profilage → statut `fallback`, échantillons bruts utilisés sans description ;
- dossier vide → description "inconnue" ;
- whitelist rechargée depuis le store : un dossier ajouté manuellement sur le serveur est invisible jusqu'au prochain run (documenté par un test).

Vert : job de découverte complet, lancé au démarrage + intervalle.

## Jalon 5 — Triage LLM (`ez-llm` + `ez-triage`)

Tests d'abord — c'est le jalon le plus dense :
- désérialisation de la réponse du triage : JSON strict vers struct `TriageDecision` (serde), champ manquant = erreur explicite, pas de panic ;
- validation whitelist : `target_folder` hors whitelist → action rejetée, logguée, mail laissé dans INBOX (règle anti-placement-fantaisiste de la spec) ;
- confiance < seuil → `action = "none"`, mail dans INBOX (test paramétré juste au seuil : 0.79 vs 0.8) ;
- le prompt contient : liste des dossiers + descriptions + exemples, batch de mails, consigne "dossier parmi cette liste, sinon INBOX" ;
- réponses du LLM mélangées → les UID sont réassociés par le `uid` fourni, jamais par l'ordre (un test retourne le JSON dans le désordre) ;
- batch slicing : 35 mails, batch 15 → 3 requêtes ;
- `is_newsletter` et `priority` plafonnés à leurs domaines (1-5, booléen) ;
- retry avec backoff sur erreur LLM, respect de `rate_limit_per_min`.

Vert : `TriageEngine` complet.

## Jalon 6 — Actions d'écriture (`ez-mail` + `ez-triage`)

Tests d'abord :
- `--dry-run` → aucune commande IMAP d'écriture émise (assertion sur le fake), `action = "dry_run"` enregistré ;
- `copy` → `COPY` IMAP émis vers le bon dossier, UID enregistré ;
- `move` → `MOVE` IMAP, INBOX jamais vidé sans l'option ;
- échec IMAP sur une action → les autres mails du lot continuent, l'échec est loggué et l'UID pas marqué traité (retenté au poll suivant) ;
- action marquée traitée **après** succès seulement.

Vert : `Executor`.

## Jalon 7 — Boucle daemon (`emailzero` binaire)

Tests d'abord :
- `--once` : une passe puis exit 0 ; INBOX vide → exit sans appel LLM ;
- intervalle : horloge injectée (fake clock), la passe suivante part après `poll_interval_secs` ;
- deux boîtes : pollées indépendamment avec leurs intervalles ;
- crash d'une boîte (fake IMAP en erreur) → l'autre continue, le daemon ne meurt pas (règle "pas de résilience forte, mais on ne s'arrête pas pour un serveur capricieux").

Vert : boucle + CLI (`run`, `--once`, `--dry-run`, `--refresh-folders`).

## Jalon 8 — Conteneur + smoke réel

- Dockerfile multi-stage → image ARM (`debian-slim` ou `scratch`), cross-compile `aarch64`.
- Compose de prod : emailzero + proton-bridge + volumes (state store, config).
- **Smoke manuel en `--dry-run`** sur Orange et Proton réels : lecture des logs JSON, revue des décisions, ajustement du prompt.
- Premier run réel en `copy` sur une boîte, puis passage à `move` par config quand le tri est jugé sain.

## Ordre et dépendances

```
0 Scaffold ──► 1 Config ──► 2 Store ──► 3 Fetcher ──► 4 Découverte ──► 5 Triage ──► 6 Actions ──► 7 Daemon ──► 8 Conteneur + smoke
```

Chaque jalon ne démarre que le précédent est vert (CI inclus). Les jalons 4 et 5 peuvent alterner avec le 6 en pratique, mais jamais un jalon ne merge avec un test rouge ou absent.

## Risques suivis

- **async-imap + Bridge** : comportements IMAP spécifiques de Bridge (UIDVALIDITY qui bouge ?) → découvert au smoke, jalon 8, avec les logs de diagnostic prévus.
- **RAM du Pi** : mesurée au jalon 8 ; si ça coince avec Bridge, repli VPS sans changement de code.
- **Qualité LLM des profils de dossiers** : si le tri est douteux en smoke, itérer sur le prompt (le prompt est un artefact versionné, pas du code).

## Notes pour plus tard (actées en revue de #4)

- `Store::pending_uids` fait un COUNT par UID (N+1) : OK à l'échelle d'une boîte perso, à rebattre en un seul `IN (...)` si le backfill historique (v2, API Batch) arrive.
- `LlmVerdict` ne porte pas le `uid` : la réassociation verdict → mail se fait dans le moteur de triage via le `uid` embarqué dans le batch (`UidVerdict` depuis le jalon 2a).
- Le chemin du fichier SQLite est dans le TOML (`[store].path`, défaut `data/email-zero.sqlite`) depuis le jalon 2a.
- Le corps des mails est borné dans le prompt via `[llm] max_body_chars` (défaut 2000, `0` = illimité, marqueur « … [tronqué] ») depuis le jalon 2a — issue #7.
