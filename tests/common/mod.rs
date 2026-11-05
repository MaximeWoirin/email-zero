//! Helpers partages entre les mini-serveurs IMAP des tests d'integration.

/// Decoupe les arguments d'un `UID FETCH` : renvoie `(uid, query)`.
pub fn split_fetch_args(args: &str) -> (u32, &str) {
    let uid = args
        .split(' ')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let query = args.split_once(' ').map(|x| x.1).unwrap_or("");
    (uid, query)
}

/// Construit la reponse a un `UID FETCH`, conforme a la RFC 3501 §6.4.5 :
/// un serveur reel ne repond jamais PEEK — `BODY.PEEK[HEADER]` produit
/// `BODY[HEADER] {n}` (en-tete seul), tout autre query (`BODY.PEEK[]`)
/// produit `BODY[] {n}` (message complet).
pub fn fetch_reply(tag: &str, uid: u32, query: &str, mail: &str) -> String {
    if query == "BODY.PEEK[HEADER]" {
        let header = format!("{}\r\n\r\n", mail.split("\r\n\r\n").next().unwrap_or(mail));
        let bytes = header.as_bytes();
        format!(
            "* 1 FETCH (UID {uid} BODY[HEADER] {{{}}}\r\n{header})\r\n{tag} OK FETCH ok\r\n",
            bytes.len()
        )
    } else {
        let body = mail.as_bytes();
        format!(
            "* 1 FETCH (UID {uid} BODY[] {{{}}}\r\n{mail})\r\n{tag} OK FETCH ok\r\n",
            body.len()
        )
    }
}
