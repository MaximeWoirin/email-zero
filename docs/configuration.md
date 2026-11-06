# Configuration

Référence complète du fichier de config TOML. [`config.example.toml`](../config.example.toml)
à la racine du dépôt est l'exemple exécutable — il est validé par un test dédié
(`tests/example_config.rs`), donc toujours à jour. Copiez-le en `config.toml`
(ignoré par Git) et remplissez les **noms** de variables d'env : aucun secret dans le
fichier de config, uniquement des références résolues au démarrage
(voir [Gestion des secrets](#gestion-des-secrets)).

## `[[mailbox]]` — boîtes (répétable, au moins une requise)

| Clé | Type | Défaut | Rôle |
|-----|------|--------|------|
| `id` | string | requis | Identifiant interne de la boîte ; doit être **unique** parmi toutes les boîtes. |
| `host` | string | requis | Serveur IMAP (ex. `imap.example.com`, ou `bridge` si les boîtes passent par Proton Bridge en compose). |
| `port` | entier | requis | Port IMAP (993 en TLS, 1143 via Bridge en clair local). |
| `username` | string | requis | Identifiant de connexion. |
| `password_env` | string | — | Nom de la variable d'environnement contenant le mot de passe. |
| `password_cmd` | string | — | Commande exécutée via `sh -c` au démarrage ; la sortie stdout (trimmée) est le mot de passe (`pass email/<boîte>`, `op read …`). |
| `password` | string | — | Mot de passe en clair — toléré (déconseillé), **warning au démarrage**. |
| `tls` | bool | `false` | Chiffrement TLS de la connexion IMAP (rustls). |
| `poll_interval_secs` | entier | `300` | Intervalle entre deux polls en mode daemon. |

**Règle** : exactement **une** source de secret parmi `password_env`,
`password_cmd`, `password` — zéro ou plusieurs = erreur de validation nommant
la boîte.

## `[llm]` — endpoint LLM (OpenAI-compatible)

| Clé | Type | Défaut | Rôle |
|-----|------|--------|------|
| `base_url` | string | requis | Racine de l'API, ex. `https://litellm.example.com/v1` ; les requêtes partent en `POST {base_url}/chat/completions`. Voir [Règles de validation](#règles-de-validation). |
| `api_key_env` | string | requis | Nom de la variable d'environnement contenant la clé API (transmise en bearer). |
| `model` | string | requis | Modèle appelé. |
| `temperature` | réel | `0.2` | Température d'échantillonnage. |
| `top_p` | réel | `1.0` | Nucleus sampling. |
| `max_tokens` | entier | `4096` | Tokens max de la réponse. |
| `reasoning_effort` | string | — | Niveau de raisonnement (`"low"`…) ; transmis tel quel, ignoré si le modèle ne le supporte pas. |
| `timeout_secs` | entier | `90` | Timeout d'une requête LLM (pas de contrainte temps réel). |
| `batch_size` | entier | `15` | Nombre de mails par requête LLM (batch). |
| `max_body_chars` | entier | `2000` | Borne du corps injecté dans le prompt (`0` = illimité, avec marqueur « … [tronqué] ») — un mail long ne doit pas faire déborder le contexte et tuer tout le batch. |
| `rate_limit_per_min` | entier | `0` | Limite de requêtes LLM par minute (`0` = illimité). |

### `[llm.retry]` — retry LLM

| Clé | Type | Défaut | Rôle |
|-----|------|--------|------|
| `max_attempts` | entier | `3` | Nombre de tentatives. |
| `backoff_base_secs` | entier | `5` | Backoff exponentiel : `base * 2^(tentative-1)` (5 s, 10 s, 20 s…), borné à 5 min. |

## `[imap]` — section IMAP

### `[imap.retry]` — retry IMAP

| Clé | Type | Défaut | Rôle |
|-----|------|--------|------|
| `max_attempts` | entier | `3` | Nombre de tentatives. |
| `backoff_base_secs` | entier | `2` | Backoff exponentiel (2 s, 4 s, 8 s…), borné à 5 min. |

## `[triage]` — décision de tri

| Clé | Type | Défaut | Rôle |
|-----|------|--------|------|
| `action` | string | `"copy"` | Action appliquée : `"copy"` ou `"move"` (les deux écrivent dans les dossiers IMAP ; `move` nettoie l'INBOX). |
| `confidence_threshold` | réel | `0.8` | Seuil de confiance dans [0, 1] : en dessous, le mail reste dans INBOX. |
| `prompt_path` | string | `"prompts/triage.md"` | Prompt système du triage (artefact versionné, isolé du code). |

## `[discovery]` — job de découverte des dossiers

| Clé | Type | Défaut | Rôle |
|-----|------|--------|------|
| `interval_secs` | entier | `86400` | Rafraîchissement du profil des dossiers (24 h). |
| `sample_per_folder` | entier | `20` | Derniers headers échantillonnés par dossier (`SEARCH ALL` + `BODY.PEEK[HEADER]`) ; `0` = désactivé. |
| `examples_in_prompt` | entier | `3` | Exemples de sujets par dossier dans le prompt de triage. |
| `exclude_folders` | liste de strings | `["Sent", "Drafts", "Trash", "Junk"]` | Dossiers exclus de la whitelist, par attribut special-use RFC 6154 ou par nom. `Archive` reste une cible légitime. INBOX est toujours exclu. |
| `prompt_path` | string | `"prompts/discovery.md"` | Prompt système du profilage LLM des dossiers. |

## `[store]` — state store

| Clé | Type | Défaut | Rôle |
|-----|------|--------|------|
| `path` | string | `"data/email-zero.sqlite"` | Fichier SQLite (créé avec son répertoire parent si absent). |

## `[log]` — logging

| Clé | Type | Défaut | Rôle |
|-----|------|--------|------|
| `level` | string | `"info"` | Niveau minimal : `trace`, `debug`, `info`, `warn`, `error`. **Priorité `RUST_LOG`** : si la variable d'environnement `RUST_LOG` est définie, elle prend le pas sur cette valeur. |

Logs en **JSON structurés** sur stdout. `[log] level = "debug"` loggue en plus
chaque requête LLM envoyée (modèle, URL, payload) et chaque réponse reçue
(status, payload) — utile pour déboguer le prompt et le contrat de triage.

## Gestion des secrets

Les secrets ne sont **jamais dans le fichier de config** — seulement des références,
résolues au démarrage de chaque boîte (une env absente ou une commande en échec =
refus de démarrer la boîte) :

- **`password_env`** : nom de la variable d'environnement (ex. `EZ_MAILBOX_PASSWORD`) ;
  le secret vit dans l'environnement du processus.
- **`password_cmd`** : commande exécutée (`pass email/<boîte>`, keyring, `op read …`).
- **`password = "..."` en clair** : toléré mais **gros warning au
  démarrage** — pour ne pas en prendre l'habitude.

Le `Debug` de la config masque tous les champs secrets (`***`) : le log de la config
chargée ne contient jamais un secret résolu (test de rédaction `tests/secrets.rs`).

## Règles de validation

Appliquées au chargement (`Config::parse`) :

- au moins une `[[mailbox]]` ;
- `id` de mailbox **uniques** (doublon = erreur) ;
- exactement une source de secret par boîte (`password_env` / `password_cmd` / `password`) ;
- `triage.action` ∈ `{"copy", "move"}` ;
- `triage.confidence_threshold` ∈ [0, 1].

`llm.base_url` est en outre validée à la **construction du client LLM**
(`validate_base_url`, `src/clients/llm.rs`), pas au parsing de la config : elle doit
commencer par `https://` — la clé API est transmise en bearer, un `http://` la
transmettrait en clair sur le réseau. Exception : `http://` accepté pour le loopback
(`127.0.0.1`, `localhost`, `::1`/`[::1]`) **et pour `host.docker.internal`** (proxy LLM
sur l'hôte Docker, ex. Ollama/LiteLLM local en compose via `extra_hosts`).

## Déploiement

### Docker Compose

```sh
cp config.example.toml config.toml       # adapter les boîtes à votre serveur IMAP
cp .env.example .env && chmod 600 .env  # secrets injectés via env_file ; .env JAMAIS commité
install -d -o 10001 -g 10001 data        # propriétaire = user non-root du conteneur
docker compose run --rm email-zero --config /config/config.toml --dry-run --once  # vérification sans écriture
docker compose up -d                     # daemon
```

Une boîte passant par Proton Bridge s'initialise une fois avec
`docker compose run --rm bridge init` (interactif).

Dans `docker-compose.yml` : `config.toml` est monté en **lecture seule**
(`./config.toml:/config/config.toml:ro`), le state store vit dans `./data`, les
secrets viennent du `.env` via `env_file`, les ports de Bridge ne sont publiés que
sur le loopback, et `extra_hosts: host.docker.internal:host-gateway` permet à la
boîte d'atteindre un proxy LLM sur l'hôte. `config.toml` et `.env` sont dans
`.gitignore`.

### Kubernetes

Même image, même config : seules les sources des secrets changent — des Secrets
K8s montés en variables d'environnement (`password_env` pointe toujours sur le nom
de la variable). La config TOML ne change pas.
