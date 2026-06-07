//! [`FileJournal`] — kaatumiskestävä append-only JSONL-journal.
//!
//! Jokainen [`JournalEntry`] kirjoitetaan yhtenä JSON-rivinä (`\n`-päätteinen)
//! tiedoston loppuun. Kirjoitus flushataan ja fsyncataan ([`std::fs::File::sync_all`])
//! ennen kuin [`append`](crate::Journal::append) palaa, joten valmistunut askel
//! on levyllä myös äkillisen kaatumisen jälkeen.
//!
//! ## Kaatumiskestävyys
//! Jos prosessi kaatuu kesken rivin kirjoituksen, viimeinen rivi voi jäädä
//! vajaaksi (ei `\n`-päätettä, tai typistynyt JSON). [`replay_from`] sietää
//! **tasan tämän yhden tapauksen**: tiedoston *viimeinen* rivi jonka jäsennys
//! epäonnistuu JA jolta puuttuu rivinvaihto hylätään hiljaisesti vajaana
//! kirjoituksena. Mikä tahansa *aiempi* vioittunut rivi on aito korruptio ja
//! palautuu [`crate::DurableError::CorruptEntry`]:nä.
//!
//! ## Itse-eheytys avattaessa (heal-on-open)
//! Vajaan viimeisen rivin **sietäminen luvussa ei riitä** — jos sitä ei poisteta
//! levyltä, seuraava [`append`](crate::Journal::append) liittyy SAMALLE
//! fyysiselle riville (koska tyngästä puuttuu `\n`), jolloin tynkä + tuore rivi
//! sulautuvat yhdeksi sisäkorruptioksi joka kaataa kaikki myöhemmät luvut.
//! Siksi [`FileJournal::open`] **typistää** tällaisen rivinvaihdottoman
//! tyngän pois avattaessa: tynkä on aina keskeneräinen (fsyncattamaton) kirjoitus
//! joka ei koskaan valmistunut, joten sen hylkääminen on turvallista JA
//! välttämätöntä, jotta append jatkuu puhtaalta rivirajalta.
//!
//! ## Object Safety
//! Metodit ottavat `&self` jotta trait on `dyn`-yhteensopiva. File-kahva on
//! `Mutex<File>`-suojassa.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::entry::{JournalEntry, StepId};
use crate::error::{DurableError, Result};
use crate::journal::Journal;

/// Levylle kirjoittava append-only JSONL-journal.
///
/// Pitää avoimen tiedostokahvan kirjoitusta varten ja muistaa polun lukua
/// varten. Avaaminen luo tiedoston jos sitä ei ole; olemassa olevaan
/// tiedostoon jatketaan (append-tila). File-kahva on Mutex-suojassa jotta
/// trait on `dyn`-yhteensopiva (`&self` metodit).
#[derive(Debug)]
pub struct FileJournal {
    path: PathBuf,
    file: Mutex<File>,
}

impl FileJournal {
    /// Avaa (tai luo) journalin annetusta polusta append-tilassa.
    ///
    /// Olemassa olevan tiedoston rivit säilyvät — uudet rivit lisätään loppuun.
    ///
    /// # Errors
    /// [`DurableError::Io`] jos tiedostoa ei voi avata/luoda.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        // Itse-eheytys ENNEN kirjoituskahvan avaamista: jos tiedoston loppuun jäi
        // kaatumisessa rivinvaihdoton tynkä, se typistetään pois. Muuten seuraava
        // append liittyisi samalle fyysiselle riville ja turmelisi journalin
        // pysyvästi (sisäkorruptio). Ks. moduulin doc "heal-on-open".
        heal_torn_trailing_fragment(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// Palauttaa journalin tiedostopolun.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lukee ja jäsentää kaikki rivit, sietäen vajaan viimeisen rivin
    /// (kaatumisen jälki). Palautuvat rivit ovat tiedostojärjestyksessä.
    fn read_all_entries(&self) -> Result<Vec<JournalEntry>> {
        let _file = self.file.lock().unwrap();
        // Create a new file handle for reading since we can't hold the lock
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);

        // Kerää (rivinumero, sisältö, oliko_rivinvaihto) jotta vajaa viimeinen
        // rivi voidaan tunnistaa luotettavasti.
        let mut raw_lines: Vec<(u64, String)> = Vec::new();
        let mut had_trailing_newline = true;
        let mut line_no: u64 = 0;
        for line in reader.lines() {
            let line = line?;
            line_no += 1;
            // `BufRead::lines` poistaa `\n`:n; emme suoraan tiedä oliko
            // viimeisellä rivillä rivinvaihtoa. Se päätellään alla erikseen.
            raw_lines.push((line_no, line));
        }

        // Selvitä päättyikö tiedosto rivinvaihtoon: jos ei, viimeinen rivi on
        // potentiaalisesti vajaa kirjoitus.
        if let Some(last_byte) = last_byte_of(&self.path)? {
            had_trailing_newline = last_byte == b'\n';
        }

        let total = raw_lines.len();
        let mut entries = Vec::with_capacity(total);
        for (idx, (line_no, content)) in raw_lines.into_iter().enumerate() {
            let is_last = idx + 1 == total;
            // Ohita tyhjät rivit (esim. ylimääräinen rivinvaihto lopussa).
            if content.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<JournalEntry>(&content) {
                Ok(entry) => entries.push(entry),
                Err(parse_err) => {
                    if is_last && !had_trailing_newline {
                        // Klassinen kaatumisen jälki: viimeinen rivi jäi vajaaksi
                        // eikä siinä ole päätös-rivinvaihtoa. Hylätään hiljaisesti.
                        continue;
                    }
                    return Err(DurableError::corrupt(
                        line_no,
                        format!("invalid json: {parse_err}"),
                    ));
                }
            }
        }
        Ok(entries)
    }
}

/// Typistää tiedoston lopusta kaatumisen jättämän rivinvaihdottoman tyngän.
///
/// Eheytys avattaessa (ks. moduulin doc): kaatuminen kesken [`append`]:in voi
/// jättää tiedoston loppuun vajaan, **rivinvaihdottoman** rivin. Sellaista riviä
/// ei koskaan fsyncattu loppuun, joten se ei ole sitoutunut askel — ja ellei sitä
/// poisteta levyltä, seuraava append liittyy sen perään SAMALLE fyysiselle
/// riville ja tuottaa pysyvän sisäkorruption.
///
/// Toiminta on **konservatiivinen**: tiedostoa typistetään vain kun
/// 1. tiedosto ei pääty `\n`:ään (eli viimeinen rivi on potentiaalisesti vajaa), JA
/// 2. tuo viimeinen (rivinvaihdoton) rivi EI jäsenny ehjäksi [`JournalEntry`]:ksi.
///
/// Jos viimeinen rivi jäsentyy ehjäksi mutta vain `\n` puuttuu (täysin
/// mahdollinen, jos kirjoitus ehti rungon mutta ei päätös-`\n`:ää — käytännössä
/// `append` kirjoittaa rivin + `\n` yhtenä `write_all`-kutsuna, mutta ollaan
/// varovaisia), riviä EI typistetä — se on validi askel ja säilytetään.
/// Tällöin lisätään pelkkä puuttuva `\n`, jotta seuraava append alkaa puhtaalta
/// riviltä rikkomatta ehjää askelta.
///
/// # Errors
/// [`DurableError::Io`] jos tiedoston luku tai typistys epäonnistuu.
fn heal_torn_trailing_fragment(path: &Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    // Olematon tai tyhjä tiedosto: ei mitään eheytettävää.
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        // Tiedostoa ei vielä ole — open luo sen myöhemmin, ei eheytettävää.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(DurableError::Io(e)),
    };
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }

    // Päättyykö tiedosto rivinvaihtoon? Jos kyllä, viimeinen rivi on ehjästi
    // päätetty eikä eheytystä tarvita.
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }

    // Tiedosto ei pääty `\n`:ään → etsi viimeisen rivin alku (edellisen `\n`:n
    // jälkeinen tavu) skannaamalla taaksepäin. Lue koko tiedosto; journalit ovat
    // rivipohjaisia eivätkä mielivaltaisen suuria yhdellä lukukerralla.
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.read_to_end(&mut bytes)?;

    // Viimeisen rivin alkuoffset = viimeisen `\n`:n jälkeinen tavu (tai 0).
    let last_line_start = match bytes.iter().rposition(|&b| b == b'\n') {
        Some(pos) => pos + 1,
        None => 0,
    };
    let last_line = &bytes[last_line_start..];

    // Tyhjä viimeinen rivi (esim. pelkkiä välilyöntejä): ei jäsennettävää askelta,
    // mutta ei myöskään tynkää jonka append turmelisi — jätetään rauhaan.
    if last_line.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }

    // Jäsentyykö viimeinen (rivinvaihdoton) rivi ehjäksi entryksi?
    if serde_json::from_slice::<JournalEntry>(last_line).is_ok() {
        // Ehjä askel jolta vain puuttuu päätös-`\n`: säilytä rivi, lisää `\n`
        // jotta seuraava append alkaa puhtaalta riviltä.
        file.seek(SeekFrom::End(0))?;
        file.write_all(b"\n")?;
    } else {
        // Vajaa tynkä: typistä se kokonaan pois → tiedosto päättyy edelliseen
        // ehjään riviin (sen `\n`:ään) tai tyhjenee. Append jatkuu puhtaasti.
        let new_len = u64::try_from(last_line_start).unwrap_or(0);
        file.set_len(new_len)?;
    }
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Palauttaa tiedoston viimeisen tavun, tai `None` jos tiedosto on tyhjä.
fn last_byte_of(path: &Path) -> Result<Option<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut buf = [0u8; 1];
    file.read_exact(&mut buf)?;
    Ok(Some(buf[0]))
}

impl Journal for FileJournal {
    fn append(&self, entry: JournalEntry) -> Result<()> {
        // Sarjallista ensin: jos serde epäonnistuu, levyä ei kosketa.
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        let mut file = self.file.lock().unwrap();
        file.write_all(line.as_bytes())?;
        file.flush()?;
        // fsync: takaa että rivi on fyysisesti levyllä ennen paluuta — tämä on
        // koko kaatumiskestävyyden ydin.
        file.sync_all()?;
        Ok(())
    }

    fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>> {
        let all = self.read_all_entries()?;
        Ok(all.into_iter().filter(|e| e.step_id >= from).collect())
    }

    fn replay_all(&self) -> Result<Vec<JournalEntry>> {
        self.read_all_entries()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Pieni RAII-temp-tiedosto ilman ulkoisia crateja.
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let unique = format!(
                "familyclaw-durable-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            );
            p.push(unique);
            // Varmista puhdas alku.
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

    #[test]
    fn open_create_append_replay_roundtrip() {
        let tmp = TempPath::new("roundtrip");
        let j = FileJournal::open(tmp.path()).expect("open");
        assert!(j.is_empty().expect("empty"));

        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append a");
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append b");

        let all = j.replay_all().expect("replay");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
    }

    #[test]
    fn reopen_persists_entries() {
        let tmp = TempPath::new("persist");
        {
            let j = FileJournal::open(tmp.path()).expect("open 1");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append");
        }
        // Uusi kahva samaan tiedostoon — simuloi prosessin restartin.
        let j2 = FileJournal::open(tmp.path()).expect("open 2");
        let all = j2.replay_all().expect("replay");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].step_name(), Some("a"));
    }

    #[test]
    fn append_continues_existing_file() {
        let tmp = TempPath::new("continue");
        {
            let j = FileJournal::open(tmp.path()).expect("open 1");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append");
        }
        let j2 = FileJournal::open(tmp.path()).expect("open 2");
        j2.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append");
        assert_eq!(j2.replay_all().expect("replay").len(), 2);
    }

    #[test]
    fn replay_from_filters() {
        let tmp = TempPath::new("from");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "s", json!(i)))
                .expect("append");
        }
        let tail = j.replay_from(StepId::new(2)).expect("replay_from");
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].step_id, StepId::new(2));
    }

    #[test]
    fn tolerates_truncated_last_line_after_crash() {
        let tmp = TempPath::new("truncated");
        {
            let j = FileJournal::open(tmp.path()).expect("open");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append");
        }
        // Simuloi kaatuminen kesken kirjoituksen: liitä vajaa JSON ILMAN
        // päätös-rivinvaihtoa.
        {
            let mut raw = OpenOptions::new()
                .append(true)
                .open(tmp.path())
                .expect("reopen raw");
            raw.write_all(b"{\"step_id\":1,\"timestamp\":\"2026")
                .expect("write partial");
            raw.flush().expect("flush");
        }
        // Replay sietää vajaan viimeisen rivin: palautuu vain ehjä ensimmäinen.
        let j = FileJournal::open(tmp.path()).expect("reopen journal");
        let all = j.replay_all().expect("replay tolerant");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].step_name(), Some("a"));
    }

    #[test]
    fn rejects_corrupt_interior_line() {
        let tmp = TempPath::new("corrupt-interior");
        {
            // Kirjoita ehjä rivi, sitten roskarivi PÄÄTÖS-rivinvaihdolla
            // (= sisäkorruptio, ei vajaa kirjoitus), sitten ehjä rivi.
            let good = serde_json::to_string(&JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("ser");
            let good2 =
                serde_json::to_string(&JournalEntry::completed(StepId::new(1), "b", json!(2)))
                    .expect("ser");
            let mut raw = OpenOptions::new()
                .create(true)
                .append(true)
                .open(tmp.path())
                .expect("open raw");
            raw.write_all(format!("{good}\n{{garbage}}\n{good2}\n").as_bytes())
                .expect("write");
            raw.flush().expect("flush");
        }
        let j = FileJournal::open(tmp.path()).expect("open");
        let err = j.replay_all().expect_err("interior corruption must error");
        match err {
            DurableError::CorruptEntry { line, .. } => assert_eq!(line, 2),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// REGRESSIO (red-team `replay_after_torn_write_leaves_journal_permanently_corrupt`):
    /// torn-write → open eheyttää tyngän → append jatkuu PUHTAALTA rivirajalta →
    /// uusi reopen + replay onnistuu ilman `CorruptEntry`:ä.
    ///
    /// Ennen korjausta: open vain sieti tyngän luvussa, mutta jätti sen levylle;
    /// seuraava append liittyi samalle riville ja tuotti sisäkorruption joka
    /// kaatoi jokaisen myöhemmän reopenin. Nyt tynkä typistetään avattaessa.
    #[test]
    fn heals_torn_trailing_fragment_so_append_does_not_corrupt() {
        let tmp = TempPath::new("heal-torn");
        // Vaihe 1: kaksi ehjää askelta levylle.
        {
            let j = FileJournal::open(tmp.path()).expect("open 1");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append a");
            j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
                .expect("append b");
        }
        // Vaihe 2: simuloi kaatuminen kesken kirjoituksen — rivinvaihdoton tynkä.
        {
            let mut raw = OpenOptions::new()
                .append(true)
                .open(tmp.path())
                .expect("reopen raw");
            raw.write_all(b"{\"step_id\":2,\"timestamp\":\"2026")
                .expect("write partial");
            raw.flush().expect("flush");
        }

        // Vaihe 3: open EHEYTTÄÄ tyngän (typistys), sitten append step c.
        {
            let j = FileJournal::open(tmp.path()).expect("open 2 heals");
            // Heti avauksen jälkeen tiedosto päättyy ehjään riviin (\n).
            let after_open = std::fs::read_to_string(tmp.path()).expect("read");
            assert!(
                after_open.ends_with('\n'),
                "heal must leave file ending in newline, got:\n{after_open}"
            );
            // Eheytetty: vain kaksi ehjää askelta jäljellä, tynkä poissa.
            assert_eq!(j.replay_all().expect("replay after heal").len(), 2);
            j.append(JournalEntry::completed(StepId::new(2), "c", json!(3)))
                .expect("append c onto healed boundary");
        }

        // Vaihe 4: reopen + replay ONNISTUU — ei sisäkorruptiota.
        let j = FileJournal::open(tmp.path()).expect("open 3");
        let all = j.replay_all().expect("replay must not be CorruptEntry");
        assert_eq!(all.len(), 3, "kaksi ehjää + tuore step c = 3 ehjää riviä");
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
        assert_eq!(all[2].step_name(), Some("c"));

        // Yksikään fyysinen rivi ei sisällä kahta step_id-avainta (= ei sulautumaa).
        let contents = std::fs::read_to_string(j.path()).expect("read final");
        assert!(
            !contents
                .lines()
                .any(|l| l.matches("\"step_id\"").count() >= 2),
            "no physical line may fuse two entries:\n{contents}"
        );
    }

    /// Eheytys EI saa typistää ehjää viimeistä riviä jolta vain puuttuu `\n`.
    /// Tällöin lisätään pelkkä puuttuva rivinvaihto ja askel säilyy.
    #[test]
    fn heal_preserves_intact_last_line_missing_only_newline() {
        let tmp = TempPath::new("heal-intact");
        // Kirjoita yksi ehjä entry RAA'asti ILMAN päätös-`\n`:ää.
        {
            let good = serde_json::to_string(&JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("ser");
            let mut raw = OpenOptions::new()
                .create(true)
                .append(true)
                .open(tmp.path())
                .expect("open raw");
            raw.write_all(good.as_bytes()).expect("write");
            raw.flush().expect("flush");
        }
        // open: rivi on ehjä → säilytetään, vain `\n` lisätään.
        let j = FileJournal::open(tmp.path()).expect("open heals newline");
        assert_eq!(j.replay_all().expect("replay").len(), 1, "ehjä rivi säilyy");
        // Append jatkuu puhtaasti.
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append b");
        let all = j.replay_all().expect("replay 2");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
    }

    #[test]
    fn empty_file_replays_empty() {
        let tmp = TempPath::new("empty");
        let j = FileJournal::open(tmp.path()).expect("open");
        assert!(j.replay_all().expect("replay").is_empty());
        assert_eq!(j.len().expect("len"), 0);
    }

    #[test]
    fn path_accessor_returns_open_path() {
        let tmp = TempPath::new("path");
        let j = FileJournal::open(tmp.path()).expect("open");
        assert_eq!(j.path(), tmp.path());
    }
}
