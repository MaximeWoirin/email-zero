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

