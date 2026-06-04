//! Suhteellisten päiväysten absolutisointi.
//!
//! `absolutize_dates`-vaihe (design §2.3, Anthropic Dreaming) ratkaisee
//! konkreettisen perheen kipupisteen: muisto "`agent_a` lähti **eilen**" muuttuu
//! merkityksettömäksi heti seuraavana päivänä, ellei "eilen" sidota
//! kalenteripäivään. Tämä moduuli korvaa suhteelliset päiväsanat
//! absoluuttisilla ISO-päivämäärillä (`YYYY-MM-DD`) suhteessa unijakson
//! viitehetkeen.
//!
//! Vertailu on tarkoituksella **konservatiivinen**: vain selkeät,
//! yksiselitteiset päiväsanat korvataan, ja korvaus tehdään vain kokonaisille
//! sanoille (ei sanan osille). Tuntematon ⇒ teksti jätetään ennalleen
//! (CLAUDE.md ydinarvo: älä arvaa).

use chrono::{Datelike, Duration};
use familyclaw_core::Timestamp;

/// Yksi tunnettu suhteellinen päiväsana ja sen siirtymä viitepäivästä.
struct RelativeWord {
    /// Sana sellaisena kuin se esiintyy tekstissä (pieniksi kirjaimiksi).
    word: &'static str,
    /// Päivien siirtymä viitepäivästä (`-1` = eilen, `+1` = huomenna).
    offset_days: i64,
}

/// Tunnetut suhteelliset päiväsanat (suomi + englanti).
///
/// Lista on tarkoituksella suppea ja yksiselitteinen — "today/tänään" ei
/// muuta merkitystä kalenteripäiväksi sidottuna, joten se sisältyy
/// täydellisyyden vuoksi (offset 0).
const RELATIVE_WORDS: &[RelativeWord] = &[
    RelativeWord {
        word: "eilen",
        offset_days: -1,
    },
    RelativeWord {
        word: "yesterday",
        offset_days: -1,
    },
    RelativeWord {
        word: "tänään",
        offset_days: 0,
    },
    RelativeWord {
        word: "today",
        offset_days: 0,
    },
    RelativeWord {
        word: "huomenna",
        offset_days: 1,
    },
    RelativeWord {
        word: "tomorrow",
        offset_days: 1,
    },
    RelativeWord {
        word: "toissapäivänä",
        offset_days: -2,
    },
    RelativeWord {
        word: "ylihuomenna",
        offset_days: 2,
    },
];

/// Muotoilee päiväyksen ISO-muotoon (`YYYY-MM-DD`) viitehetkestä siirrettynä.
///
/// Siirto tehdään päivätasolla viitehetken kalenteripäivästä. Jos siirto
/// vuotaisi chronon edustaman päiväalueen yli, viitepäivä palautetaan
/// sellaisenaan (paniikiton fallback).
fn shifted_iso(reference: Timestamp, offset_days: i64) -> String {
    let base = reference.date_naive();
    let shifted = base
        .checked_add_signed(Duration::days(offset_days))
        .unwrap_or(base);
    format!(
        "{:04}-{:02}-{:02}",
        shifted.year(),
        shifted.month(),
        shifted.day()
    )
}

/// Lopputulos yhden tekstin absolutisoinnista.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsolutizeResult {
    /// Mahdollisesti uudelleenkirjoitettu teksti.
    pub text: String,
    /// Kuinka monta päiväsanaa korvattiin.
    pub replacements: usize,
}

impl AbsolutizeResult {
    /// Muuttuiko teksti.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.replacements > 0
    }
}

/// Onko merkki sananraja (ei aakkosnumeerinen).
///
/// Käytetään kokonaisten sanojen tunnistukseen, jottei esim. "todays" tai
/// "yesterdays" osu osittain. Unicode-tietoinen, jotta ä/ö toimivat.
fn is_word_boundary(c: char) -> bool {
    !c.is_alphanumeric()
}

/// Pieniksi kirjaimiksi muunnettu yhden merkin kopio, kun muunnos on 1:1.
///
/// Lähes kaikki tunnistettavat kirjaimet (ASCII a–z + suomalaiset ä/ö/å)
/// muuntuvat yhdeksi merkiksi. Harvinaisissa tapauksissa joissa muunnos
/// laajenisi (esim. eräät erikoismerkit), palautetaan alkuperäinen merkki —
/// tällöin se vain ei osu pieneen päiväsanaan, mikä on turvallinen tulos.
fn lower_char(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(first), None) => first,
        _ => c,
    }
}

/// Korvaa kokonaiset esiintymät `needle`-sanasta (case-insensitive)
/// `replacement`-merkkijonolla. Palauttaa (teksti, korvausmäärä).
///
/// Korvaa vain sananrajojen ympäröimät esiintymät, jotta osittaiset osumat
/// (esim. "todays" sanasta "today") jäävät koskematta. Vertailu tehdään
/// merkki kerrallaan pieniksi kirjaimiksi muunnettuna, mutta **säilytetyt
/// merkit otetaan alkuperäisestä tekstistä** — joten alkuperäinen kirjainkoko
/// ja muu teksti (esim. paikkamerkit) säilyvät ennallaan.
fn replace_whole_word(haystack: &str, needle: &str, replacement: &str) -> (String, usize) {
    if needle.is_empty() {
        return (haystack.to_string(), 0);
    }
    // Alkuperäiset merkit (säilytettävät) + niiden pienaakkosversiot (vertailu).
    let orig: Vec<char> = haystack.chars().collect();
    let lower: Vec<char> = orig.iter().map(|&c| lower_char(c)).collect();
    let needle_chars: Vec<char> = needle.chars().map(lower_char).collect();
    let n = needle_chars.len();

    let mut out = String::with_capacity(haystack.len() + replacement.len());
    let mut count = 0_usize;
    let mut i = 0_usize;
    while i < orig.len() {
        let window_matches = i + n <= orig.len() && lower[i..i + n] == needle_chars[..];
        if window_matches {
            let left_ok = i == 0 || is_word_boundary(lower[i - 1]);
            let right_ok = i + n == orig.len() || is_word_boundary(lower[i + n]);
            if left_ok && right_ok {
                out.push_str(replacement);
                count += 1;
                i += n;
                continue;
            }
        }
        out.push(orig[i]);
        i += 1;
    }
    (out, count)
}

/// Korvaa kaikki tunnetut suhteelliset päiväsanat absoluuttisilla
/// ISO-päivämäärillä suhteessa viitehetkeen `reference`.
///
/// Korvausmuoto on `<sana> (YYYY-MM-DD)`, jotta alkuperäinen ilmaisu säilyy
/// luettavuuden vuoksi mutta absoluuttinen päivä on kiinnitetty. Esim.
/// `"lähti eilen"` → `"lähti eilen (2026-06-03)"`.
///
/// Jo absolutisoitu sana (jota seuraa heti `(YYYY-MM-DD)`) ohitetaan, joten
/// toistuva unijakso on idempotentti — sama muisto ei kerrytä päivämääriä.
#[must_use]
pub fn absolutize(text: &str, reference: Timestamp) -> AbsolutizeResult {
    let mut current = text.to_string();
    let mut total = 0_usize;

    for rw in RELATIVE_WORDS {
        let iso = shifted_iso(reference, rw.offset_days);
        let replacement = format!("{} ({iso})", rw.word);
        // Idempotenssi: jos sana on jo seurattu täsmälleen tällä annotaatiolla,
        // älä korvaa uudestaan. Tehdään tämä korvaamalla ensin valmis muoto
        // paikkamerkiksi, korvaamalla loput, ja palauttamalla paikkamerkki.
        let sentinel = "\u{0}DREAM_DATE\u{0}";
        let already = format!("{} ({iso})", rw.word);
        let guarded = current.replace(already.as_str(), sentinel);
        let (replaced, count) = replace_whole_word(&guarded, rw.word, &replacement);
        current = replaced.replace(sentinel, already.as_str());
        total += count;
    }

    AbsolutizeResult {
        text: current,
        replacements: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// Kiinteä viitehetki testeille: 2026-06-04 (UTC).
    fn reference() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
            .single()
            .expect("valid reference instant")
    }

    #[test]
    fn shifted_iso_handles_offsets() {
        let r = reference();
        assert_eq!(shifted_iso(r, 0), "2026-06-04");
        assert_eq!(shifted_iso(r, -1), "2026-06-03");
        assert_eq!(shifted_iso(r, 1), "2026-06-05");
        assert_eq!(shifted_iso(r, -2), "2026-06-02");
        assert_eq!(shifted_iso(r, 2), "2026-06-06");
    }

    #[test]
    fn shifted_iso_crosses_month_boundary() {
        let r = Utc
            .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
            .single()
            .expect("valid");
        assert_eq!(shifted_iso(r, -1), "2026-05-31");
    }

    #[test]
    fn absolutize_finnish_yesterday() {
        let r = reference();
        let res = absolutize("agent_a lähti eilen kotiin", r);
        assert!(res.changed());
        assert_eq!(res.replacements, 1);
        assert_eq!(res.text, "agent_a lähti eilen (2026-06-03) kotiin");
    }

    #[test]
    fn absolutize_english_tomorrow() {
        let r = reference();
        let res = absolutize("the deploy ships tomorrow", r);
        assert_eq!(res.replacements, 1);
        assert_eq!(res.text, "the deploy ships tomorrow (2026-06-05)");
    }

    #[test]
    fn absolutize_today_offset_zero() {
        let r = reference();
        let res = absolutize("we shipped it today", r);
        assert_eq!(res.text, "we shipped it today (2026-06-04)");
    }

    #[test]
    fn absolutize_multiple_words() {
        let r = reference();
        let res = absolutize("started yesterday, finishing tomorrow", r);
        assert_eq!(res.replacements, 2);
        assert!(res.text.contains("yesterday (2026-06-03)"));
        assert!(res.text.contains("tomorrow (2026-06-05)"));
    }

    #[test]
    fn absolutize_is_case_insensitive() {
        let r = reference();
        let res = absolutize("Yesterday it rained", r);
        assert_eq!(res.replacements, 1);
        // Sana normalisoituu pieniksi kirjaimiksi korvauksessa.
        assert!(res.text.contains("yesterday (2026-06-03)"));
    }

    #[test]
    fn absolutize_does_not_touch_partial_words() {
        let r = reference();
        // "yesterdays" ei ole kokonainen "yesterday" → ei kosketa.
        let res = absolutize("yesterdays news", r);
        assert_eq!(res.replacements, 0);
        assert_eq!(res.text, "yesterdays news");
    }

    #[test]
    fn absolutize_no_relative_word_is_unchanged() {
        let r = reference();
        let res = absolutize("a plain factual statement", r);
        assert!(!res.changed());
        assert_eq!(res.text, "a plain factual statement");
    }

    #[test]
    fn absolutize_is_idempotent() {
        let r = reference();
        let once = absolutize("left eilen", r);
        assert_eq!(once.replacements, 1);
        let twice = absolutize(&once.text, r);
        // Toinen ajo ei lisää uutta päivämäärää.
        assert_eq!(twice.replacements, 0);
        assert_eq!(twice.text, once.text);
    }

    #[test]
    fn absolutize_result_changed_helper() {
        let unchanged = AbsolutizeResult {
            text: "x".to_string(),
            replacements: 0,
        };
        assert!(!unchanged.changed());
        let changed = AbsolutizeResult {
            text: "x".to_string(),
            replacements: 1,
        };
        assert!(changed.changed());
    }
}
