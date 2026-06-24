//! Ristiriita-tietoinen tägäys (SleepGate-malli, arXiv 2603.14517).
//!
//! `drop_contradicted` ([`crate::contradiction`]) on **destruktiivinen**: se
//! hautaa durable-journalin ristiriitaisiksi merkitsemät muistot heti.
//! `SleepGate` ehdottaa lievempää, palautuvaa välivaihetta: kun kaksi muistoa ovat
//! ristiriidassa, **älä poista kumpaakaan heti** — *tägää* molemmat
//! `Conflicted`-tilaan ja anna myöhemmän konsolidaation (tai uudemman todisteen)
//! ratkaista kumpi jää. Tämä peilaa perheen arvoa *"verify before disagreeing"*
//! natiiviksi: ristiriita on signaali tutkia, ei käsky tuhota.
//!
//! ## Miksi tägi eikä uusi elinkaaritila
//! `Conflicted` EI ole [`familyclaw_memory::MemoryStatus`]-variantti: elinkaari
//! (`Active → Archived → Tombstoned`) elää [`familyclaw_memory`]-cratessa
//! (tämän paketin ulkopuolella) ja kuvaa *säilyvyyttä*, ei *luotettavuutta*.
//! Ristiriita on ortogonaalinen totuus — muisto voi olla yhtä aikaa `Active` ja
//! ristiriidassa toisen kanssa. Siksi merkintä tehdään lisäämällä muiston
//! `tags`-listaan vakiotägi [`CONFLICT_TAG`]; muisto säilyy täysin haettavana ja
//! koskemattomana muuten. Kun ristiriita ratkeaa, tägi voidaan poistaa ilman
//! että muiston tilaa on jouduttu palauttamaan haudasta.
//!
//! ## API
//! - [`ConflictTag`] — kone-luettava merkintä yhdestä havaitusta ristiriidasta.
//! - [`is_conflicted`] — onko muisto jo tägätty ristiriitaiseksi.
//! - [`tag_conflict`] — tägää molemmat osapuolet ja palauta [`ConflictTag`].
//! - [`clear_conflict`] — poista ristiriitatägi yhdestä muistosta (ratkaisun
//!   jälkeen).

use crate::similarity::is_near_duplicate;
use familyclaw_core::{MessageId, Result, Timestamp};
use familyclaw_memory::{Memory, MemoryStore};
use serde::{Deserialize, Serialize};

/// Vakiotägi jolla ristiriitaiseksi merkityt muistot tunnistetaan
/// [`Memory::tags`]-listasta. Geneerinen (KERROS A): ei perhe-/avain-tietoa.
pub const CONFLICT_TAG: &str = "conflicted";

/// Kone-luettava merkintä yhdestä havaitusta ristiriidasta kahden muiston
/// välillä.
///
/// `left` ja `right` ovat ristiriidassa olevat muistot (järjestys on vain
/// vakautettu esitys — ei merkitsevä). `detected` on hetki jolloin ristiriita
/// havaittiin (annetaan parametrina, ei järjestelmäkellosta — deterministinen).
///
/// `ConflictTag` itsessään on puhdas, sarjallistuva datatietue: se EI mutatoi
/// tallennusta. Mutaatio (tägäys) tehdään [`tag_conflict`]-funktiolla, joka
/// palauttaa tämän tietueen auditointia varten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConflictTag {
    /// Ristiriidan ensimmäinen osapuoli (pienempi id — vakautettu järjestys).
    pub left: MessageId,
    /// Ristiriidan toinen osapuoli (suurempi id — vakautettu järjestys).
    pub right: MessageId,
    /// Hetki jolloin ristiriita havaittiin (UTC).
    pub detected: Timestamp,
}

impl ConflictTag {
    /// Rakentaa merkinnän kahdesta muisto-id:stä ja havaintohetkestä.
    ///
    /// Osapuolet järjestetään deterministisesti (`left <= right`) jotta sama
    /// ristiriita tuottaa aina saman merkinnän riippumatta argumenttien
    /// järjestyksestä — auditointi ja deduplikointi pysyvät toistettavina.
    #[must_use]
    pub fn new(a: MessageId, b: MessageId, detected: Timestamp) -> Self {
        let (left, right) = if a <= b { (a, b) } else { (b, a) };
        Self {
            left,
            right,
            detected,
        }
    }

    /// Koskeeko tämä merkintä annettua muistoa.
    #[must_use]
    pub fn involves(&self, id: MessageId) -> bool {
        self.left == id || self.right == id
    }
}

/// Onko muisto jo tägätty ristiriitaiseksi.
#[must_use]
pub fn is_conflicted(memory: &Memory) -> bool {
    has_conflict_tag(&memory.tags)
}

/// Sisäinen apu: onko tägilistalla ristiriitatägi (case-insensitive, kuten
/// `cycle::merge_metadata_into` käsittelee tägejä).
fn has_conflict_tag(tags: &[String]) -> bool {
    tags.iter().any(|t| t.eq_ignore_ascii_case(CONFLICT_TAG))
}

/// Tägää **molemmat** ristiriidan osapuolet `Conflicted`-tilaan poistamatta
/// kumpaakaan, ja palauttaa kone-luettavan [`ConflictTag`]-merkinnän.
///
/// Idempotentti: jos osapuoli on jo tägätty, sitä ei tägätä toiseen kertaan
/// (eikä turhaa kirjoitusta tehdä). Jos jompikumpi id ei löydy tallennuksesta,
/// kyseinen osapuoli ohitetaan hiljaa (poistettu/tuntematon ⇒ ei virhe), mutta
/// merkintä palautetaan silti — havainto on tosi vaikka kohde olisi jo siivottu.
///
/// **Ei koske elinkaaritilaan eikä sisältöön** — vain `tags`-listaan lisätään
/// [`CONFLICT_TAG`]. Suojattu ydin tägätään siinä missä muutkin: tägi ei vaimena
/// eikä hautaa muistoa, joten identiteetti-ankkurin merkitseminen ristiriidan
/// osapuoleksi on vaaratonta (toisin kuin hautaaminen).
///
/// # Errors
/// [`familyclaw_core::FamilyClawError`] jos tallennuksen luku tai päivitys
/// epäonnistuu.
pub async fn tag_conflict<S>(
    store: &S,
    a: MessageId,
    b: MessageId,
    detected: Timestamp,
) -> Result<ConflictTag>
where
    S: MemoryStore + ?Sized,
{
    tag_one(store, a).await?;
    tag_one(store, b).await?;
    Ok(ConflictTag::new(a, b, detected))
}

/// Lisää ristiriitatägin yhteen muistoon, jos sitä ei vielä ole.
async fn tag_one<S>(store: &S, id: MessageId) -> Result<()>
where
    S: MemoryStore + ?Sized,
{
    let Some(mut memory) = store.get(id).await? else {
        return Ok(()); // tuntematon/poistettu osapuoli — ohita hiljaa
    };
    if has_conflict_tag(&memory.tags) {
        return Ok(()); // jo tägätty — idempotentti, ei turhaa kirjoitusta
    }
    memory.tags.push(CONFLICT_TAG.to_string());
    store.update(memory).await
}

/// Poistaa ristiriitatägin yhdestä muistosta (ristiriidan ratkettua).
///
/// Palauttaa `true` jos tägi oli ja se poistettiin, `false` jos muistoa ei
/// löytynyt tai sillä ei ollut tägiä. Poistaa kaikki tägin esiintymät
/// (case-insensitive) jos niitä on useita.
///
/// # Errors
/// [`familyclaw_core::FamilyClawError`] jos tallennuksen luku tai päivitys
/// epäonnistuu.
pub async fn clear_conflict<S>(store: &S, id: MessageId) -> Result<bool>
where
    S: MemoryStore + ?Sized,
{
    let Some(mut memory) = store.get(id).await? else {
        return Ok(false);
    };
    let before = memory.tags.len();
    memory
        .tags
        .retain(|t| !t.eq_ignore_ascii_case(CONFLICT_TAG));
    if memory.tags.len() == before {
        return Ok(false); // ei tägiä — ei kirjoitusta
    }
    store.update(memory).await?;
    Ok(true)
}

/// Etsii lähes-identtiset muistoparit ja palauttaa kustakin [`ConflictTag`]:n
/// **mutatoimatta mitään** — puhdas, sivuvaikutukseton havaintofunktio.
///
/// Tämä on [`tag_conflict`]:n lukuversio: missä `tag_conflict` *kirjoittaa*
/// tägin tallennukseen, tämä vain *raportoi* mitkä parit kannattaa tutkia.
/// Käy läpi muistot sisäkkäisellä `i < j`-silmukalla (kukin pari kerran),
/// ohittaa ei-haettavat muistot ([`Memory::is_retrievable`]) ja vertaa parin
/// `content`-kentät [`is_near_duplicate`]-funktiolla annetulla kynnyksellä.
/// Osumasta syntyy [`ConflictTag::new`] kahdesta id:stä ja `detected`-hetkestä.
///
/// **Tulos on KANDIDAATTI-lista tutkittavia lähes-duplikaattipareja, EI
/// todistettuja ristiriitoja.** Samankaltaisuus on leksikaalinen Jaccard
/// (sananjoukko-päällekkäisyys, [`similarity`](crate::similarity)) — kaksi
/// melkein samasanaista muistoa voi silti väittää eri asiaa, ja kaksi eri
/// sanoin kirjoitettua muistoa voi väittää saman. Käytä tulosta
/// konsolidaation tai myöhemmän todistepohjaisen ratkaisun *syötteenä*, älä
/// käskynä haudata.
///
/// Järjestys on deterministinen: parit tuotetaan syötteen järjestyksessä
/// (`i` kasvaa ulompana, `j` sisempänä), joten sama syöte tuottaa aina saman
/// listan samassa järjestyksessä. `detected` annetaan parametrina (ei
/// järjestelmäkellosta), kuten muuallakin tässä moduulissa.
#[must_use]
pub fn detect_conflicts(
    memories: &[Memory],
    threshold: f32,
    detected: Timestamp,
) -> Vec<ConflictTag> {
    let mut tags = Vec::new();
    for i in 0..memories.len() {
        if !memories[i].is_retrievable() {
            continue;
        }
        for other in memories.iter().skip(i + 1) {
            if !other.is_retrievable() {
                continue;
            }
            if is_near_duplicate(&memories[i].content, &other.content, threshold) {
                tags.push(ConflictTag::new(memories[i].id, other.id, detected));
            }
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use familyclaw_memory::{ImportanceFactors, LocalJsonStore, MemoryStatus};

    /// Kiinteä viitehetki: 2026-06-04 12:00 UTC (deterministinen).
    fn at() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
            .single()
            .expect("valid instant")
    }

    fn mem(content: &str) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build()
    }

    #[test]
    fn conflict_tag_orders_parties_deterministically() {
        let a = MessageId::new();
        let b = MessageId::new();
        let t1 = ConflictTag::new(a, b, at());
        let t2 = ConflictTag::new(b, a, at());
        // Sama ristiriita kummin päin tahansa → identtinen merkintä.
        assert_eq!(t1, t2);
        assert!(t1.left <= t1.right);
        assert!(t1.involves(a));
        assert!(t1.involves(b));
        assert!(!t1.involves(MessageId::new()));
    }

    #[tokio::test]
    async fn tag_conflict_keeps_both_memories() {
        let store = LocalJsonStore::in_memory();
        let id_a = store.add(mem("agent_a is in city a")).await.expect("a");
        let id_b = store.add(mem("agent_a is in city b")).await.expect("b");

        let tag = tag_conflict(&store, id_a, id_b, at()).await.expect("tag");

        // KRIITTINEN: kumpaakaan ei poistettu, molemmat yhä aktiivisia.
        let a = store.get(id_a).await.expect("g").expect("p");
        let b = store.get(id_b).await.expect("g").expect("p");
        assert_eq!(a.status, MemoryStatus::Active);
        assert_eq!(b.status, MemoryStatus::Active);
        // Molemmat tägätty ristiriitaisiksi.
        assert!(is_conflicted(&a));
        assert!(is_conflicted(&b));
        // Merkintä koskee molempia.
        assert!(tag.involves(id_a));
        assert!(tag.involves(id_b));
        assert_eq!(tag.detected, at());
    }

    #[tokio::test]
    async fn tag_conflict_is_idempotent() {
        let store = LocalJsonStore::in_memory();
        let id_a = store.add(mem("claim x")).await.expect("a");
        let id_b = store.add(mem("claim not-x")).await.expect("b");

        tag_conflict(&store, id_a, id_b, at()).await.expect("1");
        tag_conflict(&store, id_a, id_b, at()).await.expect("2");

        // Tägi esiintyy täsmälleen kerran kummassakin (ei kahdesti).
        let a = store.get(id_a).await.expect("g").expect("p");
        let count = a
            .tags
            .iter()
            .filter(|t| t.eq_ignore_ascii_case(CONFLICT_TAG))
            .count();
        assert_eq!(count, 1, "tägiä ei saa lisätä kahdesti");
    }

    #[tokio::test]
    async fn tag_conflict_preserves_existing_tags() {
        let store = LocalJsonStore::in_memory();
        let m = Memory::builder("tagged memory")
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .tags(["work".to_string(), "milestone".to_string()])
            .build();
        let id_a = store.add(m).await.expect("a");
        let id_b = store.add(mem("other side")).await.expect("b");

        tag_conflict(&store, id_a, id_b, at()).await.expect("tag");

        let a = store.get(id_a).await.expect("g").expect("p");
        assert!(a.tags.iter().any(|t| t == "work"));
        assert!(a.tags.iter().any(|t| t == "milestone"));
        assert!(is_conflicted(&a));
    }

    #[tokio::test]
    async fn tag_conflict_ignores_unknown_ids() {
        let store = LocalJsonStore::in_memory();
        let real = store.add(mem("real one")).await.expect("real");
        let ghost = MessageId::new();

        // Toinen osapuoli ei ole tallennuksessa → ohitetaan hiljaa, mutta
        // merkintä palautetaan silti ja olemassa oleva osapuoli tägätään.
        let tag = tag_conflict(&store, real, ghost, at()).await.expect("tag");
        assert!(tag.involves(real));
        assert!(tag.involves(ghost));
        assert!(is_conflicted(
            &store.get(real).await.expect("g").expect("p")
        ));
        assert!(store.get(ghost).await.expect("g").is_none());
    }

    #[tokio::test]
    async fn clear_conflict_removes_tag() {
        let store = LocalJsonStore::in_memory();
        let id_a = store.add(mem("x")).await.expect("a");
        let id_b = store.add(mem("y")).await.expect("b");
        tag_conflict(&store, id_a, id_b, at()).await.expect("tag");
        assert!(is_conflicted(
            &store.get(id_a).await.expect("g").expect("p")
        ));

        let cleared = clear_conflict(&store, id_a).await.expect("clear");
        assert!(cleared);
        assert!(!is_conflicted(
            &store.get(id_a).await.expect("g").expect("p")
        ));
        // Toinen osapuoli yhä tägätty (clear koskee yhtä muistoa kerrallaan).
        assert!(is_conflicted(
            &store.get(id_b).await.expect("g").expect("p")
        ));
    }

    #[tokio::test]
    async fn clear_conflict_on_untagged_is_noop() {
        let store = LocalJsonStore::in_memory();
        let id = store.add(mem("untagged")).await.expect("a");
        let cleared = clear_conflict(&store, id).await.expect("clear");
        assert!(!cleared, "ei tägiä → false, ei kirjoitusta");
        assert!(!clear_conflict(&store, MessageId::new())
            .await
            .expect("clear ghost"));
    }

    // ── detect_conflicts (puhdas, mutatoimaton havaintofunktio) ───────────

    #[test]
    fn detect_conflicts_empty_input_is_empty() {
        assert!(detect_conflicts(&[], 0.5, at()).is_empty());
    }

    #[test]
    fn detect_conflicts_no_near_dups_is_empty() {
        // Täysin erilliset sananjoukot → ei pareja.
        let mems = [mem("alpha beta gamma"), mem("delta epsilon zeta")];
        assert!(detect_conflicts(&mems, 0.5, at()).is_empty());
    }

    #[test]
    fn detect_conflicts_two_near_dups_gives_one_pair() {
        let m1 = mem("agent_a shipped the release today");
        let m2 = mem("agent_a shipped the release today");
        let (id1, id2) = (m1.id, m2.id);
        let mems = [m1, m2];

        let tags = detect_conflicts(&mems, 0.8, at());
        assert_eq!(tags.len(), 1);
        // Merkintä koskee molempia, vakautetussa järjestyksessä, oikealla hetkellä.
        assert!(tags[0].involves(id1));
        assert!(tags[0].involves(id2));
        assert_eq!(tags[0], ConflictTag::new(id1, id2, at()));
        assert_eq!(tags[0].detected, at());
    }

    #[test]
    fn detect_conflicts_skips_non_retrievable() {
        // Kaksi identtistä muistoa, mutta toinen haudattu → ei ole haettavissa,
        // joten paria ei synny.
        let m1 = mem("agent_a is in city a");
        let mut m2 = mem("agent_a is in city a");
        assert!(m2.tombstone(), "ei-suojattu muisto pitää voida haudata");
        assert!(!m2.is_retrievable());
        let mems = [m1, m2];

        assert!(
            detect_conflicts(&mems, 0.8, at()).is_empty(),
            "haudattu osapuoli ohitetaan"
        );
    }

    #[test]
    fn detect_conflicts_threshold_gates_pairs() {
        // Jaccard("the cat sat", "the cat ran") = 0.5.
        let m1 = mem("the cat sat");
        let m2 = mem("the cat ran");
        let mems = [m1, m2];

        // 0.5-kynnyksellä pari kelpaa, 0.6-kynnyksellä ei.
        assert_eq!(detect_conflicts(&mems, 0.5, at()).len(), 1);
        assert!(detect_conflicts(&mems, 0.6, at()).is_empty());
    }

    #[test]
    fn detect_conflicts_is_deterministic_and_ordered() {
        // Kolme lähes-identtistä → kolme paria (0,1), (0,2), (1,2)
        // syötteen järjestyksessä; sama syöte → sama lista joka ajolla.
        let m0 = mem("agent_a finished the migration");
        let m1 = mem("agent_a finished the migration");
        let m2 = mem("agent_a finished the migration");
        let (id0, id1, id2) = (m0.id, m1.id, m2.id);
        let mems = [m0, m1, m2];

        let first = detect_conflicts(&mems, 0.9, at());
        let second = detect_conflicts(&mems, 0.9, at());
        assert_eq!(first, second, "deterministinen: sama syöte → sama lista");
        assert_eq!(first.len(), 3);
        assert_eq!(first[0], ConflictTag::new(id0, id1, at()));
        assert_eq!(first[1], ConflictTag::new(id0, id2, at()));
        assert_eq!(first[2], ConflictTag::new(id1, id2, at()));
    }
}
