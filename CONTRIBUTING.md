# Contribuer

## Checklist avant d'ouvrir une PR

- [ ] Toute ligne de code de production est précédée d'un test qui échoue d'abord (TDD strict).
- [ ] La documentation est mise à jour **dans la même PR** que le changement (mapping ci-dessous).
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` et `cargo test` passent.

## Documentation à jour dans chaque PR

Un changement de comportement sans sa documentation est une PR incomplète :

| Le changement… | …met à jour |
|----------------|-------------|
| une clé de configuration, un défaut, une règle de validation | `docs/configuration.md` |
| un flag CLI, la sémantique du daemon | section « Référence CLI » du `README.md` |
| un module `src/`, le contrat de triage, la stratégie de tests | `docs/architecture.md` |
| un étage CI, une image de conteneur, une garde d'échec | `docs/architecture.md` (section « Stratégie de tests ») |
| la vue d'ensemble, le flux, les garanties de sûreté | `README.md` |

## Ce que la documentation doit contenir

La documentation décrit le projet tel qu'il est, pour un lecteur qui n'a pas suivi son histoire.

- **Générique et intemporel** : pas de vocabulaire de process interne ni de verbe de revue.
  Les cinq niveaux de tests définis dans `docs/architecture.md` (dont le « smoke réel »)
  sont le vocabulaire autorisé pour parler de tests.
- **Sans références à des issues ou des PRs** dans `README.md` et `docs/` : la traçabilité
  vit dans l'historique Git, les issues et les PRs. Des numéros de version explicites
  (ex. `BATCH_SCHEMA` v2) suffisent.
- **Sans détail d'installation personnelle** : ni comptes réels, ni matériel cible, ni
  pas-à-pas propre à une machine. Ces informations vivent dans les issues.
- **Exemples génériques** : `imap.example.com`, `EZ_MAILBOX_PASSWORD`,
  `pass email/<boîte>` — jamais les valeurs d'une boîte réelle.
- **Les sources exécutables font foi** : `--help` (généré par clap) pour le CLI,
  `config.example.toml` pour la configuration (validé par `tests/example_config.rs`).
  La prose documente la sémantique, pas un copié-collé de ces sorties.
