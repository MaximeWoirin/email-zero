use email_zero::store::{iso8601_utc, parse_iso8601_utc};

// Valeurs de reference verifiees avec `date -u -d @<secs>` (GNU coreutils),
// pas avec l'implementation testee.
#[test]
fn epoch_est_1970_01_01t00_00_00z() {
    assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(iso8601_utc(1), "1970-01-01T00:00:01Z");
}

#[test]
fn decembre_31_et_jour_suivant_changent_de_mois_et_d_annee() {
    assert_eq!(iso8601_utc(946684799), "1999-12-31T23:59:59Z");
    assert_eq!(iso8601_utc(946684800), "2000-01-01T00:00:00Z");
}

#[test]
fn annee_bissextile_2000_29_fevrier_puis_1er_mars() {
    // 2000 est bissextile (divisible par 400), contrairement a 1900/2100
    assert_eq!(iso8601_utc(951782400), "2000-02-29T00:00:00Z");
    assert_eq!(iso8601_utc(951868800), "2000-03-01T00:00:00Z");
}

#[test]
fn annee_non_bissextile_2100_passe_du_28_fev_au_1er_mars_sans_29() {
    assert_eq!(iso8601_utc(4107456000), "2100-02-28T00:00:00Z");
    assert_eq!(iso8601_utc(4107542400), "2100-03-01T00:00:00Z");
}

#[test]
fn fevrier_29_2024_a_heure_non_nulle() {
    assert_eq!(iso8601_utc(1709210096), "2024-02-29T12:34:56Z");
}

#[test]
fn fin_d_annee_2026() {
    assert_eq!(iso8601_utc(1798761599), "2026-12-31T23:59:59Z");
    assert_eq!(iso8601_utc(1798761600), "2027-01-01T00:00:00Z");
}

#[test]
fn limites_32_bits_et_annee_9999() {
    assert_eq!(iso8601_utc(2147483647), "2038-01-19T03:14:07Z");
    assert_eq!(iso8601_utc(4102444800), "2100-01-01T00:00:00Z");
    assert_eq!(iso8601_utc(253402300799), "9999-12-31T23:59:59Z");
}

#[test]
fn parse_iso8601_est_l_inverse_de_iso8601_sur_les_dates_de_reference() {
    // memes valeurs de reference que ci-dessus (verifiees avec GNU date)
    for secs in [
        0u64,
        1,
        946684799,
        946684800,
        951782400,
        951868800,
        4107456000,
        4107542400,
        1709210096,
        1798761599,
        1798761600,
        2147483647,
        4102444800,
        253402300799,
    ] {
        assert_eq!(
            parse_iso8601_utc(&iso8601_utc(secs)),
            Some(secs),
            "roundtrip pour {secs}"
        );
    }
}

#[test]
fn parse_iso8601_rejette_les_formats_casses() {
    assert_eq!(parse_iso8601_utc(""), None);
    assert_eq!(parse_iso8601_utc("1970-01-01T00:00:00"), None, "pas de Z");
    assert_eq!(parse_iso8601_utc("1970-01-01 00:00:00Z"), None, "pas de T");
    assert_eq!(parse_iso8601_utc("1970-13-01T00:00:00Z"), None, "mois 13");
    assert_eq!(parse_iso8601_utc("1970-01-32T00:00:00Z"), None, "jour 32");
    assert_eq!(parse_iso8601_utc("1970-01-01T25:00:00Z"), None, "heure 25");
    assert_eq!(
        parse_iso8601_utc("aaaa-01-01T00:00:00Z"),
        None,
        "non numerique"
    );
    assert_eq!(parse_iso8601_utc("1970-01-01T00:00:00Z "), None, "longueur");
}

#[test]
fn le_format_reste_triable_lexicographiquement() {
    // la spec repose sur le tri lexicographique des timestamps ISO 8601 :
    // longueur fixe 20, ordre chronologique = ordre des chaines
    let samples = vec![
        iso8601_utc(0),
        iso8601_utc(946684799),
        iso8601_utc(951782400),
        iso8601_utc(1798761599),
        iso8601_utc(253402300799),
    ];
    let mut sorted = samples.clone();
    sorted.sort();
    assert_eq!(
        samples, sorted,
        "les timestamps doivent etre deja tries chronologiquement"
    );
    assert!(samples.iter().all(|s| s.len() == 20 && s.ends_with('Z')));
}
