# Email Zero — Design v1

*Prétraitement d'emails par IA, headless, en Rust.*

## Intention et pourquoi

Un worker headless qui lit les nouveaux emails de plusieurs boîtes, les fait évaluer par un LLM (catégorie, criticité, résumé), et écrit le tri **directement dans le serveur mail via des dossiers IMAP**. Le client mail habituel (desktop, mobile) devient l'unique interface : pas d'UI à construire, le tri s'intègre au quotidien.

C'est le POC du projet Email Zero : il doit tourner à bas coût en attendant le cluster Kubernetes (Raspberry Pi 3 B+ 1 Go RAM ou petite VM), puis migrer tel quel vers le cluster.

## Objectifs v1

- **Deux boîtes en v1** : Orange (IMAP direct) et Proton (IMAP via Bridge). Outlook est reporté en v2 : Microsoft impose OAuth2 (XOAUTH2) sur son IMAP, ce qui alourdirait la v1 d'un flow de refresh token.
- **Détection des mails non traités** : `SEARCH UNSEEN` + state store. Le flag `SEEN` n'est jamais modifié — les mails restent non lus dans le client.
- **Évaluation LLM par batch (structured output)** : pour chaque mail — catégorie (un dossier existant), criticité (note), résumé une ligne, booléen `is_newsletter`, niveau de confiance.
- **Réutilisation des dossiers existants de la boîte** : pas de taxonomie figée. Un **job de découverte** (voir section dédiée) construit la whitelist et le profil sémantique de chaque dossier.
  L'agent ne peut placer un mail que dans un dossier de la whitelist (validation string-match côté Rust), ne crée jamais de dossier, et confiance basse → le mail reste dans INBOX.
- **Actions IMAP limitées** : copier/déplacer vers un dossier, poser des flags. Rien d'autre.
- **Mode `--dry-run`** : l'agent évalue et affiche ce qu'il ferait, n'écrit rien.
- **LLM via API OpenAI-compatible** : point d'entrée configurable (LiteLLM à terme), clé dans la config.

## Non-objectifs v1 (reportés en v2+)

- Désinscription automatique. Le mécanisme retenu pour la v2 : dossier `À désinscrire` (rempli par l'agent, lecture seule) et dossier `Désinscrire OK` — le déplacement d'un mail dans ce dossier par l'utilisateur **est** la validation ; l'agent exécute alors le `List-Unsubscribe`.
- Digest quotidien / récap (mail ou via Vibe).
- Exposition MCP.
- Toute UI web.

## Anti-goals (échec même si ça marche techniquement)

- Envoyer des emails ou effectuer des actions destructives.
- Marquer des mails comme lus (le tri ne doit rien masquer dans le quotidien).
- Verrouillage sur un fournisseur ou un cloud : l'outil reste générique IMAP + OpenAI-compatible.
- Perdre des mails : l'action par défaut est la copie vers dossier, pas la suppression.

## Contraintes

- Cible immédiate : ARM (Pi 3 B+), 1 Go de RAM partagés avec Bridge → empreinte minimale, binaire Rust statique.
- Temporaire : pas d'exigence de résilience forte. Un crash = on relance.
- Comptes réels dès le premier jour → `--dry-run` obligatoire au premier lancement.
- Config en fichier local simple (TOML), pas de secret dans le repo.

## Approches considérées

1. **Adopter un outil existant** (higgs, mailmomo-oss, ai-email-triage). Écarté : higgs est Proton-only (Bridge hardcodé, compte unique), les autres ne sont pas en Rust et ne matchent pas le tri-à-écriture-IMAP exact voulu. Servent de références d'architecture.
2. **Worker Python ou Node** (imap-tools / nodemailer + cron). Écarté : le code sera écrit majoritairement par une IA ; un langage strict, typé et avec un compilateur exigeant (Rust) produit moins de dérive et plus de revue facile.
3. **Worker headless en Rust (retenu)** : `async-imap` (IMAP async, IDLE supporté), `mail-parser` (RFC 5322/MIME), `async-openai` (SDK Rust de facto pour OpenAI et tout provider OpenAI-compatible — LiteLLM), `rusqlite` (state store), `serde` + TOML (config). Binaire unique, cross-compile ARM triviale, faible RAM.

Appel LLM : **structured output** (liste des dossiers dans le prompt, réponse JSON stricte désérialisée par `serde`), plutôt que tool calling — une requête par batch de mails, pas de round-trips, et la liste de dossiers sert de whitelist naturelle contre les placements douteux.

## Décisions prises en conversation

- **Dossiers IMAP comme interface**, pas de labels IMAP custom : les labels ne se comportent pas pareil chez Proton, Orange et Outlook ; les dossiers fonctionnent partout.
- **Détection "non traité" plutôt que "nouveau"** : `SEARCH UNSEEN` à chaque poll, dédupliqué par les UIDs déjà traités dans le state store ; `UIDVALIDITY` stocké par dossier (changement = retraitement du dossier).
- **Pull d'abord** (poll toutes les N minutes), architecture prête pour IDLE (IDLE = un pull déclenché par le serveur, même mécanique en aval).
- **Prompt de triage = produit itérable** : versionné dans le repo, isolé du code.
- **Langage : Rust.**
- **Réutilisation des dossiers existants** plutôt que taxonomie imposée : l'utilisateur trie déjà sa boîte à sa façon ; l'agent s'adapte, avec whitelist stricte + seuil de confiance → INBOX comme garde-fous.
- **Appel LLM en structured output** (batch, JSON typé), pas de tool calling en v1.
- **Outlook reporté en v2** (OAuth2/XOAUTH2 obligatoire côté Microsoft).
- **Cible POC : Raspberry Pi 3 B+**, repli VPS si la RAM ne tient pas.
- **Pas d'API Batch dans la boucle de tri** : latence ~24h incompatible avec le tri à la réception, et volume trop faible pour l'économie. Le Batch reste une **option v2 pour le backfill de l'historique** (latence sans importance, économie réelle sur un gros volume).
- **Fréquence de poll : configurable**, forcément. L'outil est un **daemon long-cours avec intervalle dans la config** (adapté à la cible conteneur), plus un mode `--once` pour une exécution ponctuelle (manuel, cron ou timer systemd). Un CLI d'installation qui interroge l'utilisateur et crée le timer systemd est noté comme **option bare-metal ultérieure** — pas dans le POC conteneurisé.
- **Action par défaut : configurable** (`copy` ou `move`). Copy par défaut pendant le prototypage ; move — le vrai comportement voulu à terme, le tri nettoie l'INBOX — activable d'un simple changement de config.
- **Seuil de confiance : configurable**, défaut **0.8** — en dessous de 80 % de confiance, le mail reste dans INBOX.
- **Logging : JSON structuré** confirmé.
- **Retry, timeout, taille de batch : configurables** (IMAP et LLM séparément). Arbitrage par défaut : backoff exponentiel, 3 tentatives ; batch ~15 mails par requête LLM ; timeout LLM généreux (90s) car pas de contrainte temps-réel.
- **Paramètres du modèle configurables** : température, top_p, max_tokens, niveau de raisonnement (`reasoning_effort`), modèle — tout passe par le TOML et l'API OpenAI-compatible.
- **Méthodologie TDD exigée pour toute l'implémentation** : aucune ligne de code de production sans test qui échoue d'abord (rouge → vert → refactor). Les frontières externes (IMAP, LLM) sont des **traits injectés**, mockés dans les tests d'intégration, pour couvrir la spec sans dépendre de vrais comptes.

## Job de découverte des dossiers — conception

Tâche interne du daemon (pas de cron séparé), qui construit la whitelist et le profil sémantique des dossiers.

**Déclenchement** : au démarrage du daemon, puis à intervalle long configurable (défaut 24h), timestamp de dernière exécution dans le state store. Flag manuel `--refresh-folders` en plus.

**Pipeline par boîte** :
1. **Mécanique** : `LIST` → dossiers + attributs *special-use* (RFC 6154). Exclusions configurables — défaut : `Sent`, `Drafts`, `Trash`, `Junk`. `Archive` reste une cible de tri légitime.
2. **Échantillonnage** : fetch des N derniers headers par dossier (défaut 20) → sujets + expéditeurs, expéditeurs les plus fréquents calculés.
3. **Profilage LLM** : **une seule requête** pour tous les dossiers de la boîte. Entrée : nom + échantillons. Sortie : par dossier, une description d'une ligne + 2-3 exemples de contenus types. Mis en cache dans le state store (nom, description, échantillons, updated_at).

**Usage dans le triage** : le prompt embarque, par dossier, nom + description + 2-3 exemples de sujets — compact et stable d'un poll à l'autre (moins de tokens, décisions cohérentes).

**Cas limites** : dossier vide → description "inconnue" (le LLM ne l'utilisera pas) ; dossier créé entre deux découvertes → hors whitelist jusqu'au prochain run (mail dans INBOX, coût nul) ; dossier renommé → réappris au prochain run ; LLM indisponible à la découverte → fallback sur échantillons bruts sans description.

## Format de config — brouillon TOML

```toml
# ---- Boîtes ----
[[mailbox]]
id = "orange"                    # identifiant interne unique
host = "imap.orange.fr"
port = 993
username = "maxime@…"
password_env = "EZ_ORANGE_PASSWORD"   # secret : variable d'env
# password_cmd = "pass email/orange"  # ou commande (keyring/pass)
tls = true
poll_interval_secs = 300

[[mailbox]]
id = "proton"
host = "127.0.0.1"               # via Proton Bridge
port = 1143
username = "maxime@…"
password_env = "EZ_PROTON_PASSWORD"
tls = false
poll_interval_secs = 300

# ---- LLM ----
[llm]
base_url = "https://litellm.…/v1"    # n'importe quel endpoint OpenAI-compatible
api_key_env = "EZ_LLM_API_KEY"
model = "…"
temperature = 0.2
top_p = 1.0
max_tokens = 4096
reasoning_effort = "low"         # ignoré si non supporté par le modèle
timeout_secs = 90
batch_size = 15                  # mails par requête
rate_limit_per_min = 0           # 0 = illimité

[llm.retry]
max_attempts = 3
backoff_base_secs = 5            # backoff exponentiel : 5s, 10s, 20s

[imap.retry]
max_attempts = 3
backoff_base_secs = 2

# ---- Triage ----
[triage]
action = "copy"                  # "copy" | "move"
confidence_threshold = 0.8       # sous ce seuil → reste dans INBOX
prompt_path = "prompts/triage.md"

# ---- Découverte des dossiers ----
[discovery]
interval_secs = 86400            # rafraîchissement du profil des dossiers
sample_per_folder = 20          # headers échantillonnés par dossier
examples_in_prompt = 3           # exemples de sujets dans le prompt de triage
exclude_folders = ["Sent", "Drafts", "Trash", "Junk"]

# ---- Divers ----
[log]
level = "info"                  # trace, debug, info, warn, error
```

## Gestion des secrets

Les secrets ne sont **jamais dans le fichier de config**. La config ne contient que des références, résolues au démarrage :

- **`password_env`** : nom de la variable d'environnement (`EZ_ORANGE_PASSWORD`) ; le binaire lit sa valeur au lancement. Le secret vit dans l'environnement du processus.
- **`password_cmd`** : commande exécutée au démarrage (`pass email/orange`, keyring, `op read …`), la sortie stdout est le secret.
- **`password = "..."` en clair** : toléré pour le POC mais **gros warning au démarrage** — pour ne pas en prendre l'habitude.

En conteneur : un fichier `.env` (chmod 600, hors repo) injecté via `env_file` dans compose. Sur le cluster K8s plus tard : des Secrets — la config ne change pas, seule la source de la variable change. Le TOML reste committable tel quel.

## Schéma SQLite

```sql
-- Une ligne par boîte configurée (synchronisée depuis le TOML au démarrage)
CREATE TABLE mailbox (
  id TEXT PRIMARY KEY
);

-- Une ligne par dossier découvert : whitelist + profil sémantique
CREATE TABLE folder (
  mailbox_id TEXT NOT NULL REFERENCES mailbox(id),
  name TEXT NOT NULL,
  description TEXT,             -- description LLM d'une ligne ("inconnue" si dossier vide)
  examples TEXT,               -- JSON : exemples de sujets inclus dans le prompt
  excluded INTEGER NOT NULL DEFAULT 0,  -- 1 = filtré (Sent, Trash…), hors whitelist
  updated_at TEXT NOT NULL,             -- ISO 8601
  PRIMARY KEY (mailbox_id, name)
);

-- UIDs déjà traités : déduplication entre les polls, gestion de UIDVALIDITY
CREATE TABLE processed (
  mailbox_id TEXT NOT NULL REFERENCES mailbox(id),
  folder TEXT NOT NULL,          -- dossier source (INBOX en v1)
  uidvalidity INTEGER NOT NULL,  -- si changement → retraitement du dossier
  uid INTEGER NOT NULL,
  processed_at TEXT NOT NULL,    -- ISO 8601
  PRIMARY KEY (mailbox_id, folder, uidvalidity, uid)
);

-- Résultats de tri : mémoire des décisions (future base des digests et du MCP)
CREATE TABLE triage_result (
  mailbox_id TEXT NOT NULL,
  folder TEXT NOT NULL,
  uidvalidity INTEGER NOT NULL,
  uid INTEGER NOT NULL,
  target_folder TEXT,            -- NULL = resté dans INBOX (confiance sous le seuil)
  priority INTEGER NOT NULL,    -- 1 (critique) à 5 (sans importance)
  summary TEXT NOT NULL,        -- résumé une ligne
  is_newsletter INTEGER NOT NULL,  -- 0/1
  confidence REAL NOT NULL,     -- 0.0 à 1.0
  action TEXT NOT NULL,         -- "none" | "copy" | "move" | "dry_run"
  created_at TEXT NOT NULL,     -- ISO 8601
  PRIMARY KEY (mailbox_id, folder, uidvalidity, uid),
  FOREIGN KEY (mailbox_id, folder, uidvalidity, uid)
    REFERENCES processed (mailbox_id, folder, uidvalidity, uid)
);

-- Historique des exécutions du job de découverte
CREATE TABLE discovery_run (
  mailbox_id TEXT NOT NULL REFERENCES mailbox(id),
  started_at TEXT NOT NULL,
  finished_at TEXT,
  status TEXT NOT NULL,          -- "ok" | "fallback" (LLM indisponible) | "error"
  folders_profiled INTEGER
);
```

Timestamps en ISO 8601 UTC pour lisibilité et tri lexicographique. Le schéma est volontairement plat et simple : c'est un POC, on normalisera si un besoin apparaît.

## Ce qui reste au builder

- Le brouillon TOML et ce schéma sont à affiner pendant l'implémentation (naming, valeurs par défaut, migrations légères).

## Déploiement (POC)

Cible : **Raspberry Pi 3 B+** en premier — si la RAM (1 Go, partagés avec Proton Bridge) ne tient pas, repli sur un petit VPS sans changement de code. Un conteneur unique (multi-stage build, image `scratch`/`debian-slim`). Migration ultérieure vers le cluster K8s sans changement de code (config par env + volume).