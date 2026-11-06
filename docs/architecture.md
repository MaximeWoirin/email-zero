# Architecture

## Vue d'ensemble

Email Zero est un daemon headless en Rust. Pour chaque boîte configurée, il poll
INBOX en IMAP, fait évaluer les nouveaux mails par un LLM (endpoint
OpenAI-compatible) et écrit le tri directement dans les dossiers IMAP de la boîte.
Le client mail habituel reste l'interface unique : pas d'UI.

```mermaid
flowchart TB
    subgraph daemon["Daemon — src/main.rs + src/runner.rs"]
        loop["Boucle de poll<br/>(--once, ou toutes les poll_interval_secs)"]
        disc["Job de découverte des dossiers<br/>src/discovery.rs"]
        pipe["Pipeline de triage<br/>src/pipeline.rs + src/triage.rs"]
    end

    store[("State store SQLite<br/>src/store.rs")]
    imap[("Serveur IMAP<br/>INBOX + dossiers de la boîte")]
    llm[("Endpoint LLM<br/>OpenAI-compatible (LiteLLM…)")]

    loop --> pipe
    disc -->|"whitelist + profils de dossiers"| pipe
    pipe <--> store
    pipe -->|"trait MailClient"| clients_imap["Client IMAP réel<br/>src/clients/imap.rs (async-imap + rustls)"]
    clients_imap --> imap
    pipe -->|"trait LlmClient"| clients_llm["Client LLM réel<br/>src/clients/llm.rs (reqwest)"]
    clients_llm --> llm
```

Les frontières externes passent par des traits injectés (`src/ports.rs`) :
`MailClient` (toutes les opérations IMAP) et `LlmClient` (toutes les requêtes LLM).
En production, les clients réels de `src/clients/` sont branchés ; en test, des fakes
in-memory ou des mini-serveurs sur loopback.

## Modules de `src/`

| Module | Responsabilité |
|--------|----------------|
| `src/main.rs` | CLI (`--config`, `--dry-run`, `--once`, `--refresh-folders`, commandes dossiers `--list-folders` / `--describe-folder` / `--reset-folder-description` / `--mailbox`), logging JSON, warning « mot de passe en clair », amorçage de la boucle daemon. |
| `src/runner.rs` | Boucle daemon : poll de chaque boîte avec reconnexion à chaque itération, déclenchement du job de découverte quand il est dû (`--refresh-folders` force). Une boîte en échec est loguée mais ne tue pas le daemon. |
| `src/pipeline.rs` | Pipeline de triage : `SEARCH UNSEEN` → dédup via le state store → batchs LLM en structured output (`BATCH_SCHEMA` v2) réassociés par `uid` → décision → `copy`/`move` (rien en `--dry-run`) → persistance `processed` + `triage_result`. |
| `src/triage.rs` | Logique de décision pure : routage du meilleur candidat de la whitelist, seuil de confiance, `TriageAction` (`none`/`copy`/`move`/`dry_run`). |
| `src/discovery.rs` | Job de découverte des dossiers : `LIST` + attributs special-use RFC 6154, exclusions configurables, échantillonnage des derniers headers (`SEARCH ALL` + `BODY.PEEK[HEADER]`), **une seule requête LLM** de profilage par boîte (`PROFILING_SCHEMA`, prompt `prompts/discovery.md`), profils mis en cache dans `folder`. Les dossiers dont la description est en source `user` (saisie manuelle) sont **épargnés** : ni échantillonnés, ni envoyés au LLM, leur ligne `folder` reste intacte. |
| `src/store.rs` | State store SQLite : tables `mailbox` / `folder` / `processed` / `triage_result` / `discovery_run`, gestion de l'`UIDVALIDITY` (changement = retraitement du dossier). La table `folder` porte `description_source` (`llm`/`user`, migration `ALTER TABLE` à l'ouverture des anciennes bases) : une source `user` n'est jamais réécrite par `set_folder_profile`. |
| `src/folders.rs` | Commandes de gestion manuelle des dossiers, hors daemon (state store seul, sans IMAP ni LLM) : `--list-folders` (état : source, whitelist, description, exemples), `--describe-folder` (description saisie en texte libre, source `user`), `--reset-folder-description` (retour à la source `llm`). |
| `src/config.rs` | Parsing et validation du TOML, références de secrets (`password_env` / `password_cmd` / `password`), `Debug` masqué des champs secrets. |
| `src/ports.rs` | Traits injectés `MailClient` / `LlmClient` et types échangés (`Folder`, `MailHeader`, `ParsedMail`, `JsonChatRequest`…). |
| `src/clients/imap.rs` | Client IMAP réel (`async-imap` + rustls + racines webpki) : fetchs en `BODY.PEEK[]` pour ne jamais poser `\Seen`, `UIDVALIDITY` lu sur la réponse SELECT. |
| `src/clients/llm.rs` | Client LLM réel (`reqwest`) : `POST {base_url}/chat/completions`, validation de `base_url` (https exigé, loopback et `host.docker.internal` exceptés). |
| `src/retry.rs` | Backoff exponentiel configuré (`max_attempts`, `backoff_base_secs`), fonction pure testable sans dormir. |
| `src/error.rs` | Type d'erreur unique (`thiserror`). |

## Contrat de triage

Le LLM reçoit un batch de mails (whitelist des dossiers avec description et
exemples + mails avec leur `uid`) et répond en JSON strict (`BATCH_SCHEMA` v2) :
un verdict par mail, chaque verdict portant la **liste ordonnée des dossiers
plausibles** (`candidates`, au moins un élément, ordonnés par confiance décroissante).

Routage appliqué côté Rust (`src/triage.rs`), pour chaque mail :

- **Meilleur candidat de la whitelist retenu** : confiance maximale, à égalité le
  premier de la liste (`MeilleurCandidat`).
- Meilleur candidat sous le seuil configuré, ou aucun candidat de la whitelist :
  le mail reste dans INBOX (`SeuilNonAtteint`).
- Validation **tolérante** de la réponse LLM — jamais d'échec de poll :
  - `uid` inconnu du batch → verdict écarté (`UidInconnuEcarte`) ;
  - deux verdicts pour un même `uid` → candidats fusionnés (`DoublonFusionne`) ;
  - `uid` du batch sans verdict → mail laissé en place et marqué traité
    (`PasDeVerdict`).
- La cible doit appartenir à la whitelist (string-match exact côté Rust) : jamais
  de création de dossier, jamais de placement hors liste.
- **Log de routage par mail** : une ligne avec `uid`, destination, confiance,
  nombre de candidats et règle appliquée.

Le prompt de triage vit dans `prompts/triage.md` — c'est un artefact versionné,
isolé du code.

## Stratégie de tests à cinq niveaux

1. **Unitaires** (rapides) : parsing config, logique `UIDVALIDITY`, validation
   whitelist, seuil de confiance.
2. **Intégration** (le cœur) : pipeline complet — fake IMAP rempli de mails, fake
   LLM avec réponses scriptées, assertions sur les actions produites et le contenu
   du state store. Zéro réseau (`tests/pipeline.rs`, `tests/runner.rs`…).
3. **Conformité RFC (CI, boucle locale)** : le vrai client IMAP
   (`src/clients/imap.rs`) contre un vrai serveur — sidecar conteneur
   `dovecot/dovecot:2.3.21.1` **épinglé**, publié sur le loopback uniquement
   (`127.0.0.1:1143`), compte factice, fixtures maildir committées
   (`tests/fixtures/maildir/`), config `tests/dovecot/dovecot.conf`, suite
   `#[ignore]` dans `tests/imap_dovecot.rs`, job CI `imap-dovecot`. Garde d'échec
   explicite : si Dovecot n'écoute pas après 30 s, le job échoue avec les logs du
   conteneur. Objectif : attraper en CI les divergences fake/serveur réel
   (`BODY.PEEK[HEADER]`, `\Seen` jamais posé, COPY/MOVE/UIDVALIDITY) avant le smoke.
4. **E2E LLM (CI, boucle locale)** : le vrai client LLM (`src/clients/llm.rs`)
   contre un vrai proxy LiteLLM — sidecar
   `ghcr.io/berriai/litellm:v1.108.0` **épinglé**, loopback uniquement
   (`127.0.0.1:4000`), configuré en `mock_response` (`tests/litellm/config.yaml`),
   suite `#[ignore]` dans `tests/llm_litellm.rs`, job CI `llm-litellm`. Garde
   d'échec explicite après 60 s. Surface d'API réelle (bearer, codes d'erreur natifs
   du proxy 400/429/500, format de réponse) sans fournisseur, sans réseau externe,
   sans secret : `mock-ok` (batch complet), `mock-missing-verdict` (uid en
   double/omis) et un test pipeline (fake IMAP + proxy réel). Objectif :
   attraper en CI les divergences fake/proxy réel sur la réponse LLM.
5. **Smoke réel (manuel, hors CI)** : `--dry-run --once` contre des boîtes
   réelles. Jamais d'écriture en CI, jamais de secret dans les tests.

Commandes :

```sh
cargo test                                      # niveaux 1 + 2
cargo test --test imap_dovecot -- --ignored     # niveau 3 (sidecar Dovecot requis)
cargo test --test llm_litellm -- --ignored      # niveau 4 (sidecar LiteLLM requis)
```

Chaque test est nommé d'après une décision de la spec design. La CI exige
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` et `cargo test` :
aucun merge avec un warning.

## Décisions d'architecture clés

Renvoi à la [spec design v1](superpowers/specs/2026-11-01-email-zero-design.md) pour
le raisonnement complet :

- **Dossiers IMAP comme interface**, pas de labels custom : les dossiers se
  comportent pareil chez tous les fournisseurs.
- **« Non traité » plutôt que « nouveau »** : `SEARCH UNSEEN` à chaque poll, dédup
  par les UIDs déjà traités dans le state store, `UIDVALIDITY` stocké par dossier
  (changement = retraitement).
- **Structured output plutôt que tool calling** : une requête par batch de mails,
  pas de round-trips, et la liste des dossiers dans le prompt sert de whitelist
  naturelle contre les placements douteux.
- **Action `copy` par défaut** : `move` — le vrai comportement voulu à terme —
  s'active par un changement de config.
- **Pull d'abord** (poll toutes les N minutes), architecture prête pour IDLE.

## Décisions d'implémentation notables

Choix pris pendant le développement, historiquement des écarts au cadrage initial :

- **Crate unique** au lieu d'un workspace à plusieurs crates : moins de cérémonie
  et une compilation plus rapide.
- **Durcissement des secrets** : `Debug` custom sur `MailboxConfig` masquant les
  champs secrets (`***`, test de rédaction dans `tests/secrets.rs`) ; lecture de
  l'env **injectée** dans `resolve_password(&dyn Fn(&str) -> Option<String>)` (aucun
  `std::env::set_var`/`unsafe` dans les tests, qui tournent en parallèle dans le
  même processus) ; `password_cmd` implémenté (`sh -c`, sortie stdout trimmée) ;
  warning « mot de passe en clair » branché au chargement de la config dans `main`.
- **`BODY.PEEK[]` partout** : jamais de `\Seen` posé, ni sur les headers ni sur le
  corps (fetch de l'échantillonnage de découverte inclus).
- **`base_url` https exigé hors loopback** — la clé API part en bearer — `host.docker.internal`
  est aussi accepté en http (proxy LLM sur l'hôte Docker, via `extra_hosts`) ;
  `127.0.0.1` / `localhost` / `::1` restent acceptés en http pour les tests et les
  proxys locaux.
- **Pas de `rustfmt.toml` / `clippy.toml`** : les defaults suffisent.

## Notes techniques différées

- `Store::pending_uids` fait un COUNT par UID (N+1) : acceptable à l'échelle d'une
  ou deux boîtes personnelles, à rebattre en un seul `IN (...)` si le traitement
  de l'historique arrive.
- Traitement de l'historique via API Batch : envisagé pour plus tard — latence ~24 h
  incompatible avec le tri à la réception, mais économie réelle sur un gros
  volume ; hors de la boucle de tri.
