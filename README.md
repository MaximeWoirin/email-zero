# Email Zero

Prétraitement d'emails par IA, headless, en Rust : un worker qui lit les nouveaux
mails en IMAP, les fait évaluer par un LLM (catégorie, criticité, résumé) et
écrit le tri directement dans les dossiers de la boîte. Le client mail habituel
reste l'interface unique — pas d'UI.

## Docs

- [Spec design v1](docs/superpowers/specs/2026-11-01-email-zero-design.md)
- [Plan d'implémentation — TDD](docs/superpowers/specs/2026-11-01-email-zero-plan.md)

## Statut

POC en cours de construction (jalon 0 : scaffold + CI). Méthodologie : TDD
stricte, frontières IMAP/LLM en traits injectés.
