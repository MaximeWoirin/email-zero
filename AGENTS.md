# Instructions pour agents IA

Consignes pour tout agent (ou humain assisté) intervenant sur ce dépôt. Le README
décrit le projet vu de l'extérieur ; ce fichier décrit comment y travailler.
Pour la checklist de PR et le mapping « changement -> document », voir
[CONTRIBUTING.md](CONTRIBUTING.md).

## Carte du dépôt

- `README.md` — vitrine : vue d'ensemble, démarrage rapide, référence CLI.
- `docs/architecture.md` — composants, contrat de triage, stratégie de tests.
- `docs/configuration.md` — référence TOML, secrets, déploiement.
- `docs/superpowers/specs/2026-11-01-email-zero-design.md` — spec de conception.
- `src/` — code ; les frontières externes (IMAP, LLM) sont des traits injectés
  (`src/ports.rs`), testables par des fakes.
- `prompts/triage.md` — prompt envoyé au LLM.
- `config.example.toml` — exemple de configuration ; `config.toml` est ignoré
  par Git (secrets).

## Commandes

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

`cargo test` couvre les niveaux sans réseau externe. Les suites `#[ignore]`
(conformité IMAP sur Dovecot, E2E LLM sur LiteLLM) tournent dans la CI en
conteneurs ; leurs conditions d'exécution locale sont décrites dans
[docs/architecture.md](docs/architecture.md). Le cinquième niveau (smoke réel
sur une boîte existante) reste manuel et hors CI.

## Règles non négociables

1. TDD strict : tout changement de comportement commence par un test qui échoue.
2. Toute PR qui modifie un comportement met à jour la documentation dans la même
   PR ; le mapping changement -> document est dans CONTRIBUTING.md.
3. Les sources exécutables font foi : `--help` et `config.example.toml` priment
   sur la prose. Relire la source avant de documenter une option ou une clé.
4. Aucun secret ni configuration réelle dans le dépôt : les mots de passe sont
   référencés par des variables d'environnement nommées dans le TOML.
5. La CI doit rester verte sur tous ses étages (tests, conteneurs IMAP et LLM,
   build arm64, analyse statique) avant toute fusion.

## Contenu de la documentation

Lors de toute écriture ou modification de documentation :

- Générique et intemporel : décrire le projet tel qu'il est, sans narratif de
  développement ni vocabulaire de revue.
- Aucune référence aux issues, PRs ou revues : la traçabilité vit dans GitHub,
  pas dans la prose. Le vocabulaire des tests est celui des cinq niveaux décrits
  dans `docs/architecture.md`.
- Aucun détail d'installation personnelle (matériel cible, fournisseurs,
  instances). Exemples génériques : `imap.example.com`, `EZ_MAILBOX_PASSWORD`,
  `pass email/<boîte>`.
- Privilégier les blocs exécutables ou les renvois vers la source plutôt que
  les copies de code.

## Mode de contribution

Une branche dédiée par changement, une PR unique, messages de commit
conventionnels (`feat:`, `fix:`, `docs:`, `test:`, `refactor:`). Les décisions
de conception se consignent dans `docs/architecture.md` ou la spec, jamais
uniquement dans un message de commit.
