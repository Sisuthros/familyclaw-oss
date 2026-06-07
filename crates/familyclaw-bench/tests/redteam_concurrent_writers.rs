//! RED-TEAM [concurrent-writers] — kahden samanaikaisen kirjoittajan hyökkäys
//! `FileJournal`-journalia ja `LocalJsonStore`-tallennusta vastaan.
//!
//! Väite jota vastaan hyökätään: *"remembers everything, side-effects exactly
//! once"*. Jos kaksi prosessia/kahvaa kirjoittaa samaan journaliin tai samaan
//! storeen lähes yhtä aikaa, häviääkö dataa, lomittuvatko rivit (korruptio),
//! vai katoaako päivityksiä (lost update)?
//!
//! Nämä testit AJAVAT hyökkäyksen oikeaa koodia vastaan — ne eivät arvaa.
//! Mitään ei korjata: tämä on pelkkä hyökkäys + todiste.
//!
//! Kaikki kellonajat injektoidaan (`Timestamp`), ei lueta järjestelmäkelloa
//! testilogiikassa (paitsi yksilöivissä temp-poluissa, mikä on sallittua).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;

use familyclaw_durable::{FileJournal, Journal, JournalEntry, StepId};
use familyclaw_memory::{ImportanceFactors, LocalJsonStore, Memory, MemoryStore};
use serde_json::json;

/// Yksilöivä temp-polku ilman ulkoisia crateja.
fn temp_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let unique = format!(
        "familyclaw-redteam-cw-{tag}-{}-{}.dat",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    p.push(unique);
    let _ = std::fs::remove_file(&p);
    p
}

/// RAII-siivous.
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        // store-toteutuksen tmp-tiedosto.
        let mut tmp = self.0.as_os_str().to_os_string();
        tmp.push(".tmp");
        let _ = std::fs::remove_file(PathBuf::from(tmp));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HYÖKKÄYS 1: FileJournal — kaksi kahvaa, kaksi säiettä, sama tiedosto.
//
// Kumpikin säie avaa OMAN FileJournal-kahvansa samaan polkuun (kaksi erillistä
// File-deskriptoria append-tilassa, kuten kaksi prosessia tekisi) ja appendaa
// N riviä. Append tekee write_all + flush + sync_all. Kysymys: säilyykö rivien
// rakenne (ei lomitusta), ja säilyykö rivien LUKUMÄÄRÄ (ei kadonneita)?
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn filejournal_two_handles_concurrent_append_integrity() {
    const N: u64 = 400;

    let path = temp_path("journal");
    let _cleanup = Cleanup(path.clone());

    let barrier = Arc::new(Barrier::new(2));

    let mut handles = Vec::new();
    for writer_id in 0u64..2 {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let j = FileJournal::open(&path).expect("open journal handle");
            // Synkronoi molemmat säikeet alkamaan yhtä aikaa → maksimaalinen
            // lomitusriski.
            barrier.wait();
            for i in 0..N {
                // step_id koodaa kirjoittajan + indeksin jotta voimme laskea
                // tarkalleen kuinka monta kummankin riviä selvisi.
                let step = StepId::new(writer_id * 1_000_000 + i);
                let entry = JournalEntry::completed(
                    step,
                    format!("w{writer_id}"),
                    // Tukeva hyötykuorma jotta serialisoitu rivi on iso (useita
                    // sataa tavua) → write_all joutuu mahdollisesti useaan
                    // syscalliin → lomitusriski kasvaa.
                    json!({
                        "writer": writer_id,
                        "index": i,
                        "pad": "X".repeat(512),
                    }),
                );
                j.append(entry).expect("append must not fail");
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread panicked");
    }

    // ── Verifiointi: lue raakatiedosto ja erottele ehjät rivit. ─────────────
    let raw = std::fs::read_to_string(&path).expect("read journal");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();

    let total_written = 2 * usize::try_from(N).expect("N fits usize");

    // (a) Rivimäärä: jokaisen \n-päätteisen rivin pitäisi olla yksi append.
    //     Jos rivejä on vähemmän kuin 2N → kadonneita kirjoituksia (lost write).
    //     Jos parse epäonnistuu jollain rivillä → lomittunut korruptio.
    let mut parsed_ok = 0usize;
    let mut parse_failures: Vec<(usize, String)> = Vec::new();
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut duplicates = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        match serde_json::from_str::<JournalEntry>(line) {
            Ok(entry) => {
                parsed_ok += 1;
                if !seen.insert(entry.step_id.index()) {
                    duplicates += 1;
                }
            }
            Err(e) => {
                let preview: String = line.chars().take(80).collect();
                parse_failures.push((idx, format!("{e} | line='{preview}'")));
            }
        }
    }

    eprintln!(
        "[journal] lines={} parsed_ok={} parse_failures={} duplicates={} expected={}",
        lines.len(),
        parsed_ok,
        parse_failures.len(),
        duplicates,
        total_written
    );
    if let Some((idx, msg)) = parse_failures.first() {
        eprintln!("[journal] FIRST CORRUPT LINE idx={idx}: {msg}");
    }

    // (b) replay_all kautta — sama polku jota tuotanto käyttää. Sisäkorruptio
    //     (lomittunut rivi joka EI ole viimeinen) palauttaa CorruptEntry-virheen.
    let replay = FileJournal::open(&path).expect("reopen").replay_all();
    match &replay {
        Ok(entries) => eprintln!("[journal] replay_all OK, {} entries", entries.len()),
        Err(e) => eprintln!("[journal] replay_all ERROR: {e}"),
    }

    // ── Väitteet (todiste, ei arvaus). ──────────────────────────────────────
    // Väite "remembers everything" rikkoutuu jos:
    //   - rivejä katosi (parsed_ok + parse_failures < 2N), TAI
    //   - jokin rivi on korruptoitunut (parse_failures > 0), TAI
    //   - replay_all kaatuu korruptioon.
    assert!(
        parse_failures.is_empty(),
        "INTEGRITY BREAK: {} interleaved/corrupt lines in journal (first: {:?})",
        parse_failures.len(),
        parse_failures.first()
    );
    assert!(
        replay.is_ok(),
        "INTEGRITY BREAK: replay_all failed on concurrently-written journal: {:?}",
        replay.err()
    );
    assert_eq!(
        parsed_ok,
        total_written,
        "LOST WRITE: only {parsed_ok} of {total_written} appends survived (lines on disk={})",
        lines.len()
    );
    assert_eq!(duplicates, 0, "unexpected duplicate step ids: {duplicates}");
}

// ─────────────────────────────────────────────────────────────────────────────
// HYÖKKÄYS 2: LocalJsonStore — kaksi kahvaa SAMAAN polkuun, lost update.
//
// Tämä on klassinen read-modify-write-kilpa: kahdella LocalJsonStore-oliolla on
// ERILLISET RwLock + HashMap. Kumpikin lataa tiedoston (tyhjä), lisää oman
// muistonsa, ja persistoi (tmp + rename). Viimeinen rename voittaa → toinen
// muisto katoaa levyltä, vaikka add() palautti Ok.
//
// Väite "side-effects exactly once / remembers everything" testataan: jos
// molemmat add() onnistuvat mutta levyllä on vain yksi → DATA LOSS.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn localjsonstore_two_handles_same_path_lost_update() {
    let path = temp_path("store");
    let _cleanup = Cleanup(path.clone());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");

    let added_ids: Vec<familyclaw_core::MessageId> = rt.block_on(async {
        // Kaksi erillistä kahvaa samaan tiedostoon — kuten kaksi prosessia tai
        // kaksi rinnakkaista taskia jotka kumpikin avasivat storen.
        let store_a = Arc::new(LocalJsonStore::open(&path).await.expect("open a"));
        let store_b = Arc::new(LocalJsonStore::open(&path).await.expect("open b"));

        let mem_a = Memory::builder("MEMORY FROM WRITER A — must not be lost")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-a")
            .build();
        let mem_b = Memory::builder("MEMORY FROM WRITER B — must not be lost")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-b")
            .build();
        let id_a = mem_a.id;
        let id_b = mem_b.id;

        // Aja molemmat add()-kutsut samanaikaisesti.
        let ta = {
            let store_a = Arc::clone(&store_a);
            tokio::spawn(async move { store_a.add(mem_a).await })
        };
        let tb = {
            let store_b = Arc::clone(&store_b);
            tokio::spawn(async move { store_b.add(mem_b).await })
        };
        let ra = ta.await.expect("join a").expect("add a returned Ok");
        let rb = tb.await.expect("join b").expect("add b returned Ok");
        assert_eq!(ra, id_a);
        assert_eq!(rb, id_b);

        vec![id_a, id_b]
    });

    // ── Verifiointi: avaa storen levyltä UUDESTAAN (puhdas kahva). ──────────
    // Molempien add() palautti Ok ⇒ "muistettu". Mutta mitä levyllä oikeasti on?
    let on_disk = rt.block_on(async {
        let reopened = LocalJsonStore::open(&path).await.expect("reopen");
        reopened.all().await.expect("all")
    });

    let surviving_ids: std::collections::HashSet<_> = on_disk.iter().map(|m| m.id).collect();
    eprintln!(
        "[store] added=2 (both returned Ok), on_disk={} ids_present={:?}",
        on_disk.len(),
        on_disk.iter().map(|m| m.source.clone()).collect::<Vec<_>>()
    );

    // Väite: jos kumpikaan id ei kadonnut, store on turvallinen samanaikaisille
    // kahvoille. Jos toinen katosi → LOST UPDATE (side-effect "muista X" hävisi).
    let lost: Vec<_> = added_ids
        .iter()
        .filter(|id| !surviving_ids.contains(id))
        .collect();

    assert!(
        lost.is_empty(),
        "LOST UPDATE: {} of 2 concurrently-added memories vanished from disk \
         despite add() returning Ok. on_disk={}, lost_ids={:?}. \
         Two LocalJsonStore handles to the same path do NOT coordinate — \
         last rename() wins.",
        lost.len(),
        on_disk.len(),
        lost
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// HYÖKKÄYS 2b: deterministinen lost-update ILMAN kilpa-ajoitusta.
//
// Poistaa ajoitusepävarmuuden: sekvensoi read-modify-write käsin niin että
// lost update on PAKKO tapahtua jos toteutus ei koordinoi kahvoja. Tämä on
// "smoking gun" — ei flaky, todistaa rakenteellisen aukon.
// ─────────────────────────────────────────────────────────────────────────────
#[test]
fn localjsonstore_deterministic_lost_update() {
    let path = temp_path("store-det");
    let _cleanup = Cleanup(path.clone());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let (id_a, id_b, on_disk_len, a_present, b_present) = rt.block_on(async {
        // Molemmat kahvat avataan kun tiedosto on vielä tyhjä → kummankin
        // sisäinen HashMap on tyhjä (sama lähtötila kuin todellisessa kilvassa).
        let store_a = LocalJsonStore::open(&path).await.expect("open a");
        let store_b = LocalJsonStore::open(&path).await.expect("open b");

        let mem_a = Memory::builder("A")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-a")
            .build();
        let mem_b = Memory::builder("B")
            .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
            .source("writer-b")
            .build();
        let id_a = mem_a.id;
        let id_b = mem_b.id;

        // A kirjoittaa ensin (tiedostossa nyt {A}), sitten B kirjoittaa oman
        // tyhjän snapshotinsa päälle (tiedostossa nyt {B}). A katoaa levyltä.
        store_a.add(mem_a).await.expect("add a Ok");
        store_b.add(mem_b).await.expect("add b Ok");

        let reopened = LocalJsonStore::open(&path).await.expect("reopen");
        let all = reopened.all().await.expect("all");
        let a_present = all.iter().any(|m| m.id == id_a);
        let b_present = all.iter().any(|m| m.id == id_b);
        (id_a, id_b, all.len(), a_present, b_present)
    });

    eprintln!(
        "[store-det] on_disk_len={on_disk_len} A_present={a_present} B_present={b_present} \
         id_a={id_a} id_b={id_b}"
    );

    // Molemmat add() palauttivat Ok ⇒ kumpikin "muistettiin". Levyllä pitäisi
    // olla 2. Jos on 1 → todistettu rakenteellinen lost update.
    assert_eq!(
        on_disk_len, 2,
        "DETERMINISTIC LOST UPDATE: both add() returned Ok but disk has {on_disk_len} \
         memory(ies). A_present={a_present} B_present={b_present}. The second handle's \
         rename() clobbered the first handle's write — no cross-handle coordination."
    );
}
