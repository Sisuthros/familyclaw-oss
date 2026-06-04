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

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::entry::{JournalEntry, StepId};
use crate::error::{DurableError, Result};
use crate::journal::Journal;

/// Levylle kirjoittava append-only JSONL-journal.
///
/// Pitää avoimen tiedostokahvan kirjoitusta varten ja muistaa polun lukua
/// varten. Avaaminen luo tiedoston jos sitä ei ole; olemassa olevaan
/// tiedostoon jatketaan (append-tila).
#[derive(Debug)]
pub struct FileJournal {
    path: PathBuf,
    file: File,
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
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        Ok(Self { path, file })
    }

    /// Palauttaa journalin tiedostopolun.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lukee ja jäsentää kaikki rivit, sietäen vajaan viimeisen rivin
    /// (kaatumisen jälki). Palautuvat rivit ovat tiedostojärjestyksessä.
    fn read_all_entries(&self) -> Result<Vec<JournalEntry>> {
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
    fn append(&mut self, entry: JournalEntry) -> Result<()> {
        // Sarjallista ensin: jos serde epäonnistuu, levyä ei kosketa.
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        // fsync: takaa että rivi on fyysisesti levyllä ennen paluuta — tämä on
        // koko kaatumiskestävyyden ydin.
        self.file.sync_all()?;
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
        let mut j = FileJournal::open(tmp.path()).expect("open");
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
            let mut j = FileJournal::open(tmp.path()).expect("open 1");
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
            let mut j = FileJournal::open(tmp.path()).expect("open 1");
            j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
                .expect("append");
        }
        let mut j2 = FileJournal::open(tmp.path()).expect("open 2");
        j2.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append");
        assert_eq!(j2.replay_all().expect("replay").len(), 2);
    }

    #[test]
    fn replay_from_filters() {
        let tmp = TempPath::new("from");
        let mut j = FileJournal::open(tmp.path()).expect("open");
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
            let mut j = FileJournal::open(tmp.path()).expect("open");
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
