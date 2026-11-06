use std::sync::Arc;

use async_imap::types::NameAttribute;
use async_imap::{Client, Session};
use futures_util::TryStreamExt;
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

use crate::config::{MailboxConfig, RetryConfig};
use crate::error::{Error, Result};
use crate::ports::{Folder, MailClient, MailHeader, ParsedMail};
use crate::retry::{backoff_delay, sleep_tokio};

/// Trait objet du stream (async-imap exige Read + Write + Unpin + Debug + Send).
pub trait DebugStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug> DebugStream
    for T
{
}

/// Session IMAP boxee sur un stream quelconque (TLS ou TCP clair).
pub type BoxSession = Session<Box<dyn DebugStream>>;

/// Wrapper de stream : un EOF (0 octet lu) est transforme en erreur io
/// `UnexpectedEof`. Sans cela, async-imap 0.12 traite un EOF avec buffer vide
/// comme une fin de stream PROPRE (`Poll::Ready(None)`) : `UID SEARCH` sur une
/// connexion fermee cote serveur retournerait `Ok([])` au lieu d'une erreur, et
/// la perte de connexion ne serait detectee qu'en TLS (erreur rustls
/// "peer closed connection without sending TLS close_notify"). Avec ce wrapper,
/// toute fermeture serveur (TLS ou TCP clair) remonte `Error::Io` ->
/// `ImapConnectionLost` (issue #30).
#[derive(Debug)]
struct EofAsError<S> {
    inner: S,
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for EofAsError<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &r {
            if buf.filled().len() == before {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connexion fermee par le serveur (EOF)",
                )));
            }
        }
        r
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for EofAsError<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Construit le connecteur TLS (rustls + racines webpki + provider ring).
pub fn tls_connector() -> Result<TlsConnector> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Connecte et authentifie une session IMAP reelle : TLS si `mb.tls` (port 993),
/// TCP clair sinon (ex: Proton Bridge en local, port 1143). Le mot de passe est
/// resolu hors de cette fonction (jamais loggue).
/// Delai global applique a chaque phase de la connexion (TCP, handshake TLS,
/// greeting + LOGIN) : une connexion qui bloque ne doit pas figer le daemon.
pub const CONNECT_PHASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub async fn connect(mb: &MailboxConfig, password: &str) -> Result<BoxSession> {
    let addr = format!("{}:{}", mb.host, mb.port);
    let tcp = tokio::time::timeout(CONNECT_PHASE_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| Error::Imap(format!("timeout TCP {addr} ({CONNECT_PHASE_TIMEOUT:?})")))?
        .map_err(|e| Error::Imap(format!("connexion TCP {addr}: {e}")))?;
    if mb.tls {
        let connector = tls_connector()?;
        let server_name = ServerName::try_from(mb.host.clone())
            .map_err(|e| Error::Imap(format!("nom de serveur TLS invalide {}: {e}", mb.host)))?;
        let tls = tokio::time::timeout(CONNECT_PHASE_TIMEOUT, connector.connect(server_name, tcp))
            .await
            .map_err(|_| {
                Error::Imap(format!(
                    "timeout handshake TLS {addr} ({CONNECT_PHASE_TIMEOUT:?})"
                ))
            })?
            .map_err(|e| Error::Imap(format!("handshake TLS {addr}: {e}")))?;
        let mut client = Client::new(Box::new(EofAsError { inner: tls }) as Box<dyn DebugStream>);
        let _greeting = tokio::time::timeout(CONNECT_PHASE_TIMEOUT, client.read_response())
            .await
            .map_err(|_| Error::Imap(format!("timeout greeting ({CONNECT_PHASE_TIMEOUT:?})")))?
            .map_err(|e| Error::Imap(format!("greeting: {e}")))?;
        let session =
            tokio::time::timeout(CONNECT_PHASE_TIMEOUT, client.login(&mb.username, password))
                .await
                .map_err(|_| Error::Imap(format!("timeout login ({CONNECT_PHASE_TIMEOUT:?})")))?
                .map_err(|(e, _c)| Error::Imap(format!("login {}: {e}", mb.username)))?;
        Ok(session)
    } else {
        let mut client = Client::new(Box::new(EofAsError { inner: tcp }) as Box<dyn DebugStream>);
        let _greeting = tokio::time::timeout(CONNECT_PHASE_TIMEOUT, client.read_response())
            .await
            .map_err(|_| Error::Imap(format!("timeout greeting ({CONNECT_PHASE_TIMEOUT:?})")))?
            .map_err(|e| Error::Imap(format!("greeting: {e}")))?;
        let session =
            tokio::time::timeout(CONNECT_PHASE_TIMEOUT, client.login(&mb.username, password))
                .await
                .map_err(|_| Error::Imap(format!("timeout login ({CONNECT_PHASE_TIMEOUT:?})")))?
                .map_err(|(e, _c)| Error::Imap(format!("login {}: {e}", mb.username)))?;
        Ok(session)
    }
}

fn parse_raw_mail(raw: &[u8], uid: u32) -> Result<ParsedMail> {
    let msg = mail_parser::MessageParser::new()
        .parse(raw)
        .ok_or_else(|| Error::Imap(format!("uid {uid}: parsing RFC 5322 impossible")))?;
    let from = msg
        .from()
        .and_then(|l| l.first())
        .map(|a| match (a.name(), a.address()) {
            (Some(n), Some(e)) => format!("{n} <{e}>"),
            (None, Some(e)) => e.to_string(),
            _ => String::new(),
        })
        .unwrap_or_default();
    let subject = msg.subject().unwrap_or("").to_string();
    let date = msg.date().map(|d| d.to_rfc3339()).unwrap_or_default();
    let body_text = msg.body_text(0).map(|b| b.into_owned()).unwrap_or_default();
    Ok(ParsedMail {
        header: MailHeader {
            subject,
            from,
            date,
        },
        body_text,
    })
}

/// Traduit une erreur async-imap en `Error` : une erreur de transport (io, EOF/BYE
/// du serveur) devient `ImapConnectionLost` (session morte, non retentable ici —
/// le runner reconnecte, issue #30) ; NO/BAD/parse restent des `Error::Imap`.
fn map_imap_err(e: async_imap::error::Error) -> Error {
    use async_imap::error::Error as ImapError;
    match e {
        ImapError::Io(io) => Error::ImapConnectionLost(format!("io: {io}")),
        ImapError::ConnectionLost => {
            Error::ImapConnectionLost("connexion fermee par le serveur".into())
        }
        other => Error::Imap(other.to_string()),
    }
}

/// Boucle de retry inlinee : semantique identique a `retry::with_retry` (max_attempts,
/// backoff exponentiel borne, warn loggue, testee dans tests/retry.rs) mais ecrite en
/// macro bloc pour pouvoir emprunter `&mut self.session` a chaque tentative — une
/// fonction generique avec closure ne compile pas sur ce borrow.
/// Chaque tentative est bornee par `self.read_timeout` (issue #30) : une lecture
/// bloquee (serveur qui a ferme sans close_notify) echoue vite au lieu d'attendre
/// les timers TCP (~4 min 30 observees). Une `ImapConnectionLost` (timeout, io,
/// BYE) n'est PAS retentee sur la meme session : elle remonte pour reconnexion.
macro_rules! retried {
    ($self:ident, $session:ident, $body:block) => {{
        let retry = $self.retry.clone();
        let timeout = $self.read_timeout;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let result: Result<_> = {
                let $session: &mut BoxSession = &mut $self.session;
                if timeout.is_zero() {
                    $body
                } else {
                    match tokio::time::timeout(timeout, async { $body }).await {
                        Ok(r) => r,
                        Err(_) => Err(Error::ImapConnectionLost(format!(
                            "timeout de {timeout:?} sur une operation IMAP (session probablement morte)"
                        ))),
                    }
                }
            };
            match result {
                Ok(v) => break Ok(v),
                Err(e) => {
                    if matches!(e, Error::ImapConnectionLost(_)) {
                        break Err(e);
                    }
                    if attempt >= retry.max_attempts.max(1) {
                        break Err(e);
                    }
                    let delay = backoff_delay(&retry, attempt);
                    tracing::warn!(
                        attempt,
                        max_attempts = retry.max_attempts,
                        delay_ms = delay.as_millis() as u64,
                        error = %e,
                        "operation IMAP echouee, nouvelle tentative apres backoff"
                    );
                    sleep_tokio(delay).await;
                }
            }
        }
    }};
}

/// Client IMAP reel : une session par instance, dossier selectionne a la volee.
/// Le flag SEEN n'est jamais modifie (aucun STORE, fetch de corps uniquement).
pub struct ImapMailClient {
    session: BoxSession,
    retry: RetryConfig,
    /// Borne de chaque operation IMAP (commande + lecture de reponse), issue #30 ;
    /// `Duration::ZERO` = desactive.
    read_timeout: std::time::Duration,
    selected: Option<String>,
    /// UIDVALIDITY lue sur la reponse du SELECT du dossier courant.
    uidvalidity_cache: Option<u32>,
}

impl ImapMailClient {
    pub fn new(session: BoxSession, retry: RetryConfig, read_timeout: std::time::Duration) -> Self {
        Self {
            session,
            retry,
            read_timeout,
            selected: None,
            uidvalidity_cache: None,
        }
    }

    async fn select(&mut self, folder: &str) -> Result<()> {
        if self.selected.as_deref() == Some(folder) {
            return Ok(());
        }
        let folder_arg = folder.to_string();
        // RFC 3501 §6.3.1 : la reponse SELECT porte deja `OK [UIDVALIDITY n]`
        // — on la met en cache, aucun STATUS supplementaire n'est necessaire
        // (la RFC deconseille d'ailleurs STATUS sur le dossier selectionne).
        let mb = retried!(self, session, {
            session
                .select(folder_arg.clone())
                .await
                .map_err(map_imap_err)
        })?;
        self.uidvalidity_cache = mb.uid_validity;
        self.selected = Some(folder.to_string());
        Ok(())
    }
}

impl MailClient for ImapMailClient {
    async fn list_folders(&mut self) -> Result<Vec<Folder>> {
        let names = retried!(self, session, {
            let stream = session.list(None, Some("*")).await.map_err(map_imap_err)?;
            stream.try_collect::<Vec<_>>().await.map_err(map_imap_err)
        })?;
        let mut out = Vec::with_capacity(names.len());
        for n in names {
            if n.attributes().contains(&NameAttribute::NoSelect) {
                continue;
            }
            let special_use = n
                .attributes()
                .iter()
                .find_map(|a| match a {
                    NameAttribute::All => Some("\\All"),
                    NameAttribute::Archive => Some("\\Archive"),
                    NameAttribute::Drafts => Some("\\Drafts"),
                    NameAttribute::Flagged => Some("\\Flagged"),
                    NameAttribute::Junk => Some("\\Junk"),
                    NameAttribute::Sent => Some("\\Sent"),
                    NameAttribute::Trash => Some("\\Trash"),
                    _ => None,
                })
                .map(str::to_string);
            out.push(Folder {
                name: n.name().to_string(),
                special_use,
            });
        }
        Ok(out)
    }

    async fn uidvalidity(&mut self, folder: &str) -> Result<u32> {
        // UIDVALIDITY deja portee par la reponse du SELECT (mis en cache par
        // select()) : zero aller-retour supplementaire, et pas de STATUS sur
        // le dossier selectionne (deconseille par RFC 3501 §6.3.2).
        self.select(folder).await?;
        self.uidvalidity_cache.ok_or_else(|| {
            Error::Imap(format!(
                "UIDVALIDITY absent de la reponse SELECT pour {folder}"
            ))
        })
    }

    async fn search_unseen(&mut self, folder: &str) -> Result<Vec<u32>> {
        self.select(folder).await?;
        let set = retried!(self, session, {
            session.uid_search("UNSEEN").await.map_err(map_imap_err)
        })?;
        let mut uids: Vec<u32> = set.into_iter().collect();
        uids.sort_unstable();
        Ok(uids)
    }

    async fn search_all(&mut self, folder: &str) -> Result<Vec<u32>> {
        self.select(folder).await?;
        let set = retried!(self, session, {
            session.uid_search("ALL").await.map_err(map_imap_err)
        })?;
        let mut uids: Vec<u32> = set.into_iter().collect();
        uids.sort_unstable();
        Ok(uids)
    }

    async fn fetch_headers(&mut self, folder: &str, uid: u32) -> Result<MailHeader> {
        // BODY.PEEK[HEADER] : on ne telecharge que l'en-tete (echantillonnage
        // du jalon 2c), et PEEK ne pose jamais \Seen.
        self.select(folder).await?;
        let raw = retried!(self, session, {
            let stream = session
                .uid_fetch(uid.to_string(), "BODY.PEEK[HEADER]")
                .await
                .map_err(map_imap_err)?;
            let fetches: Vec<_> = stream.try_collect().await.map_err(map_imap_err)?;
            fetches
                .into_iter()
                .find(|f| f.uid == Some(uid))
                // RFC 3501 §6.4.5 : la reponse a BODY.PEEK[HEADER] est BODY[HEADER],
                // lue via header() — body() ne porte que BODY[]/RFC822 et renverrait None.
                .and_then(|f| f.header().map(|b| b.to_vec()))
                .ok_or_else(|| {
                    Error::Imap(format!("uid {uid}: en-tete absent de la reponse FETCH"))
                })
        })?;
        let msg = mail_parser::MessageParser::new()
            .parse(&raw)
            .ok_or_else(|| Error::Imap(format!("uid {uid}: parsing RFC 5322 impossible")))?;
        let from = msg
            .from()
            .and_then(|l| l.first())
            .map(|a| match (a.name(), a.address()) {
                (Some(n), Some(e)) => format!("{n} <{e}>"),
                (None, Some(e)) => e.to_string(),
                _ => String::new(),
            })
            .unwrap_or_default();
        Ok(MailHeader {
            subject: msg.subject().unwrap_or("").to_string(),
            from,
            date: msg.date().map(|d| d.to_rfc3339()).unwrap_or_default(),
        })
    }

    async fn fetch_body(&mut self, folder: &str, uid: u32) -> Result<ParsedMail> {
        self.select(folder).await?;
        let raw = retried!(self, session, {
            // BODY.PEEK[] : RFC 3501 §6.4.5 — un FETCH RFC822 implicite poserait
            // \Seen ; PEEK lit sans poser aucun flag (décision spec : SEEN jamais modifié).
            let stream = session
                .uid_fetch(uid.to_string(), "BODY.PEEK[]")
                .await
                .map_err(map_imap_err)?;
            let fetches: Vec<_> = stream.try_collect().await.map_err(map_imap_err)?;
            fetches
                .into_iter()
                .find(|f| f.uid == Some(uid))
                .and_then(|f| f.body().map(|b| b.to_vec()))
                .ok_or_else(|| Error::Imap(format!("uid {uid}: corps absent de la reponse FETCH")))
        })?;
        parse_raw_mail(&raw, uid)
    }

    async fn copy(&mut self, folder: &str, uid: u32, target: &str) -> Result<()> {
        self.select(folder).await?;
        let target_arg = target.to_string();
        retried!(self, session, {
            session
                .uid_copy(uid.to_string(), &target_arg)
                .await
                .map_err(map_imap_err)
        })
    }

    async fn r#move(&mut self, folder: &str, uid: u32, target: &str) -> Result<()> {
        self.select(folder).await?;
        let target_arg = target.to_string();
        retried!(self, session, {
            session
                .uid_mv(uid.to_string(), &target_arg)
                .await
                .map_err(map_imap_err)
        })
    }

    async fn noop(&mut self) -> Result<()> {
        retried!(self, session, {
            session.noop().await.map_err(map_imap_err)
        })
    }
}
