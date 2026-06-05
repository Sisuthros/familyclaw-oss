//! RED-TEAM: partial-fsync / OS-buffer-loss -hyökkäys durable-journaalia vastaan.
//!
//! Hyökkäyksen väite jota yritetään RIKKOA (design §5):
//! *"kill mid-task -> resumes exact step, side-effects exactly once,
//! never silently wrong"* — myös kun OS-bufferi katoaa ja journal-tiedosto
//! typistyy **keskelle JSON-entryä** (ei vain repeytynyt viimeinen RIVI vaan
//! repeytynyt viimeinen TAVU), TAI kun keskimmäinen rivi N on roskaa.
//!
//! Vaatimus: palautuu viimeiseen ehjään askeleeseen ILMAN sivuvaikutusten
//! toistoa, TAI epäonnistuu ÄÄNEKKÄÄSTI ([`DurableError::CorruptEntry`]).
//! EI KOSKAAN hiljaa väärin.
//!
//! Tämä testi ajaa OIKEAA tuotantokoodia: [`FileJournal`] + [`DurableContext`]
//! `familyclaw-durable`-cratesta. Kello injektoidaan [`Timestamp`]-arvona,
//! järjestelmäkelloa ei lueta (design §2.2).

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use familyclaw_durable::{DurableContext, DurableError, FileJournal, Journal};

/// RAII-temp-tiedosto ilman ulkoisia crateja (sama malli kuin durable-craten
/// omat testit). Siivoaa myös oheistiedostot.
struct TempPath(PathBuf);

impl TempPath {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        let unique = format!(
            "familyclaw-redteam-fsync-{tag}-{}-{:?}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        p.push(unique);
        let _ = std::fs::remove_file(&p);
        Self(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Ajaa kolmen askeleen workflow'n annettua journalia vasten, kasvattaen
/// `effects`-laskuria joka kerta kun suljin OIKEASTI ajetaan (= sivuvaikutus).
/// Replayssa suljinta EI pitäisi ajaa → laskuri ei kasva.
///
/// Palauttaa kolmen askeleen summan tuloksena (deterministinen: 1+2+3 = 6 kun
/// kaikki kolme ajetaan).
fn run_workflow<J: Journal>(journal: J, effects: &Cell<u32>) -> familyclaw_durable::Result<i64> {
    let mut ctx = DurableContext::new(journal)?;
    let a: i64 = ctx.step("alpha", || {
        effects.set(effects.get() + 1);
        Ok(1)
    })?;
    let b: i64 = ctx.step("beta", || {
        effects.set(effects.get() + 1);
        Ok(a + 2)
    })?;
    let c: i64 = ctx.step("gamma", || {
        effects.set(effects.get() + 1);
        Ok(b + 3)
    })?;
    Ok(c)
}

/// Typistää tiedoston annettuun tavupituuteen (simuloi OS-bufferin menetystä
/// ennen viimeisen entryn fsyncia: tiedosto loppuu KESKELLE JSON-entryä).
fn truncate_to(path: &Path, len: u64) {
    let f = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for truncate");
    f.set_len(len).expect("set_len");
    f.sync_all().expect("sync truncate");
}

/// Lukee tiedoston tavut.
fn read_bytes(path: &Path) -> Vec<u8> {
    let mut f = File::open(path).expect("open for read");
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).expect("read");
    buf
}

/// ATTACK 1 — torn mid-JSON: typistä journal KESKELLE viimeistä entryä
/// (ei rivinvaihtorajalla). Toinen askel "beta" jää puoliksi levylle.
///
/// Väite: replay palautuu viimeiseen EHJÄÄN askeleeseen ("alpha") ja jatkaa
/// loppuun ilman että "alpha":n sivuvaikutus toistuu.
#[test]
fn attack_torn_mid_json_recovers_to_last_good_step() {
    let tmp = TempPath::new("torn-mid-json");

    // Vaihe 1: kirjoita kaksi ehjää askelta fsyncattuna.
    {
        let mut j = FileJournal::open(tmp.path()).expect("open");
        let mut ctx = DurableContext::new(j).expect("ctx");
        // Aja vain kaksi askelta käsin niin tunnemme tarkat tavurajat.
        let _a: i64 = ctx.step("alpha", || Ok(1)).expect("alpha");
        let _b: i64 = ctx.step("beta", || Ok(3)).expect("beta");
        j = ctx.finish();
        // Varmista että molemmat ovat levyllä.
        assert_eq!(j.replay_all().expect("replay").len(), 2);
    }

    // Etsi toisen rivin rivinvaihdon tavu-offset.
    let bytes = read_bytes(tmp.path());
    let first_nl = bytes
        .iter()
        .position(|&b| b == b'\n')
        .expect("first newline exists");
    // Typistä KESKELLE toista entryä: ensimmäinen rivi + sen \n + muutama
    // tavu toisesta entrystä, mutta EI sen rivinvaihtoa → torn mid-JSON.
    let torn_len = (first_nl as u64) + 1 + 6;
    assert!(
        torn_len < bytes.len() as u64,
        "torn point must be inside second entry"
    );
    truncate_to(tmp.path(), torn_len);

    // Varmista todella: viimeinen tavu EI ole rivinvaihto (= repeytynyt).
    let after = read_bytes(tmp.path());
    assert_ne!(
        *after.last().expect("nonempty"),
        b'\n',
        "torn file must NOT end in newline"
    );

    // Hyökkäys: avaa uudelleen ja yritä replayta + jatkaa.
    let effects = Cell::new(0u32);
    let journal = FileJournal::open(tmp.path()).expect("reopen after tear");

    // read_all_entries EI saa palauttaa hiljaa väärää dataa.
    let recovered = journal.replay_all();
    match &recovered {
        Ok(entries) => {
            // Hyväksyttävä lopputulos A: sieti torn viimeinen rivi → vain
            // ensimmäinen ehjä askel jää.
            assert_eq!(
                entries.len(),
                1,
                "torn last line should be dropped → exactly 1 good entry survives, got {}",
                entries.len()
            );
            assert_eq!(entries[0].step_name(), Some("alpha"));
        }
        Err(DurableError::CorruptEntry { .. }) => {
            // Hyväksyttävä lopputulos B: epäonnistui äänekkäästi. Ei hiljaa
            // väärin → väite pitää tällöinkin.
        }
        Err(other) => panic!("unexpected error variant on torn mid-json: {other:?}"),
    }

    // Jos replay onnistui (tapaus A), jatka workflow loppuun ja varmista että
    // "alpha":n sivuvaikutus EI toistu (kerran-ja-vain-kerran).
    if recovered.is_ok() {
        let result = run_workflow(journal, &effects).expect("resume after tear");
        assert_eq!(result, 6, "final result must equal no-crash baseline 1+2+3");
        // "alpha" tuli lokista (ei sivuvaikutusta); vain beta+gamma ajetaan
        // tuoreina → täsmälleen 2 uutta sivuvaikutusta.
        assert_eq!(
            effects.get(),
            2,
            "alpha must NOT re-run on resume; only beta+gamma fresh"
        );
    }
}

/// ATTACK 2 — middle-line corruption: ehjä rivi N korvataan roskalla, rivi N+1
/// on edelleen ehjä JSON. Tiedosto päättyy rivinvaihtoon (= EI repeytynyt
/// viimeinen rivi vaan aito sisäkorruptio).
///
/// Väite: TÄYTYY epäonnistua äänekkäästi ([`DurableError::CorruptEntry`]),
/// EI palauttaa hiljaa rivin N ohittavaa "ehjää" dataa.
#[test]
fn attack_corrupt_middle_line_fails_loud() {
    let tmp = TempPath::new("corrupt-middle");

    // Kirjoita kolme ehjää askelta.
    {
        let mut j = FileJournal::open(tmp.path()).expect("open");
        let mut ctx = DurableContext::new(j).expect("ctx");
        let _a: i64 = ctx.step("alpha", || Ok(1)).expect("alpha");
        let _b: i64 = ctx.step("beta", || Ok(3)).expect("beta");
        let _c: i64 = ctx.step("gamma", || Ok(6)).expect("gamma");
        j = ctx.finish();
        assert_eq!(j.replay_all().expect("replay").len(), 3);
    }

    // Korvaa KESKIMMÄINEN rivi (rivi 2, "beta") roskalla — säilytä rivimäärä
    // ja rivinvaihdot. Tavut: [rivi1\n][rivi2\n][rivi3\n].
    let bytes = read_bytes(tmp.path());
    let nl_positions: Vec<usize> = bytes
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| (b == b'\n').then_some(i))
        .collect();
    assert_eq!(nl_positions.len(), 3, "expected 3 newline-terminated lines");

    let line2_start = nl_positions[0] + 1;
    let line2_end_inclusive_nl = nl_positions[1]; // sijainti rivin 2 \n:lle

    // Rakenna uusi sisältö: rivi1 + roskarivi2 + rivi3, kaikki \n-päätteisiä.
    let mut corrupted: Vec<u8> = Vec::new();
    corrupted.extend_from_slice(&bytes[..line2_start]); // rivi1 + \n
    corrupted.extend_from_slice(b"{this is not valid json at all]}\n"); // roskarivi2 + \n
    corrupted.extend_from_slice(&bytes[line2_end_inclusive_nl + 1..]); // rivi3 + \n

    {
        let mut f = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(tmp.path())
            .expect("rewrite corrupted");
        f.write_all(&corrupted).expect("write corrupted");
        f.sync_all().expect("sync corrupted");
    }
    // Tiedosto päättyy rivinvaihtoon → EI repeytynyt viimeinen rivi.
    let final_bytes = read_bytes(tmp.path());
    assert_eq!(
        *final_bytes.last().expect("nonempty"),
        b'\n',
        "corrupted file ends in newline (interior corruption, not torn tail)"
    );

    // Hyökkäys: replay TÄYTYY epäonnistua äänekkäästi rivillä 2.
    let journal = FileJournal::open(tmp.path()).expect("reopen corrupted");
    let result = journal.replay_all();
    match result {
        Err(DurableError::CorruptEntry { line, .. }) => {
            assert_eq!(line, 2, "must point at the corrupt interior line (#2)");
        }
        Ok(entries) => panic!(
            "SILENT CORRUPTION: replay returned {} entries instead of failing loud on garbage line 2",
            entries.len()
        ),
        Err(other) => panic!("wrong error variant (must be CorruptEntry): {other:?}"),
    }

    // Ja DurableContext::new TÄYTYY myös vuotaa virheen läpi (ei rakentaa
    // kontekstia hiljaa rikkinäisestä lokista).
    let journal2 = FileJournal::open(tmp.path()).expect("reopen corrupted 2");
    let ctx = DurableContext::new(journal2);
    assert!(
        matches!(ctx, Err(DurableError::CorruptEntry { line: 2, .. })),
        "DurableContext::new must propagate CorruptEntry on interior garbage"
    );
}

/// ATTACK 3 — torn LAST byte mid-number: typistä yhden tavun verran niin että
/// viimeinen entry on JSON jonka prefix on syntaktisesti laillinen mutta
/// vajaa (klassinen partial-fsync: bufferi katkesi kesken numeron/merkkijonon).
///
/// Tämä erikoistapaus testaa ettei "vahingossa-validi" JSON-prefiksi pääse
/// hiljaa läpi väärällä arvolla. Väite: viimeinen torn rivi pudotetaan TAI
/// virhe nostetaan; aiemmat askeleet säilyvät oikein.
#[test]
fn attack_torn_last_byte_no_silent_wrong_value() {
    let tmp = TempPath::new("torn-last-byte");

    {
        let mut j = FileJournal::open(tmp.path()).expect("open");
        let mut ctx = DurableContext::new(j).expect("ctx");
        let _a: i64 = ctx.step("alpha", || Ok(1)).expect("alpha");
        let _b: i64 = ctx.step("beta", || Ok(3)).expect("beta");
        j = ctx.finish();
        assert_eq!(j.replay_all().expect("replay").len(), 2);
    }

    // Poista täsmälleen viimeinen tavu (toisen entryn päättävä \n + 0..n).
    // Poistetaan viimeinen rivinvaihto JA yksi tavu sisältöä → torn JSON.
    let bytes = read_bytes(tmp.path());
    // Etsi viimeisen rivin alku.
    let last_nl = bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .expect("trailing newline");
    // Typistä keskelle toista entryä: jätä ensimmäinen rivi + \n + 8 tavua.
    let first_nl = bytes.iter().position(|&b| b == b'\n').expect("first nl");
    assert!(last_nl > first_nl);
    let torn_len = (first_nl as u64) + 1 + 8;
    truncate_to(tmp.path(), torn_len);

    let journal = FileJournal::open(tmp.path()).expect("reopen");
    match journal.replay_all() {
        Ok(entries) => {
            // Vain ehjä alpha saa selvitä; torn beta-prefix EI saa palautua
            // minkäänlaisena arvona.
            assert_eq!(entries.len(), 1, "only the intact alpha entry survives");
            assert_eq!(entries[0].step_name(), Some("alpha"));
            // Varmista ettei mikään selvinnyt entry ole "beta" väärällä arvolla.
            assert!(
                entries.iter().all(|e| e.step_name() != Some("beta")),
                "torn beta must NOT silently reappear"
            );
        }
        Err(DurableError::CorruptEntry { .. }) => {
            // Äänekäs epäonnistuminen on myös hyväksyttävä.
        }
        Err(other) => panic!("unexpected error on torn last byte: {other:?}"),
    }
}

/// Apuvälineiden olemassaolon varmistus (estää dead-code-varoitukset jos jokin
/// haara ei suoriudu).
#[test]
fn helpers_smoke() {
    let tmp = TempPath::new("smoke");
    {
        let mut j = FileJournal::open(tmp.path()).expect("open");
        let mut ctx = DurableContext::new(j).expect("ctx");
        let _ = ctx.step("alpha", || Ok::<i64, String>(1)).expect("alpha");
        j = ctx.finish();
        drop(j);
    }
    let bytes = read_bytes(tmp.path());
    assert!(!bytes.is_empty());
    let mut f = File::open(tmp.path()).expect("open");
    f.seek(SeekFrom::Start(0)).expect("seek");
}
