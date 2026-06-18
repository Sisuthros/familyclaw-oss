//! [`FileJournal`] — kaatumiskestävä append-only JSONL-journal.
//!
//! Jokainen [`JournalEntry`] kirjoitetaan yhtenä JSON-rivinä (`\n`-päätteinen)
//! tiedoston loppuun. Kirjoitus flushataan ja fsyncataan ([`std::fs::File::sync_all`])
//! ennen kuin [`append`](crate::Journal::append) palaa, joten valmistunut askel
//! on levyllä myös äkillisen kaatumisen jälkeen.
//!
//! ## Kaatumiskestävyys
//! Jos prosessi kaatuu kesken rivin kirjoituksen, viimeinen rivi voi jäädä
//! vajaaksi (ei `\n`-päätettä, tai typistynyt JSON). [`replay_from`](crate::Journal::replay_from) sietää
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
//! ## Tiivistys (compaction) — [`FileJournal::rewrite`]
//! Append-only-loki kasvaa rajatta jos sen päälle rakennettu tila (esim.
//! odottavat hyväksynnät, jatkettavat vuorot) kirjaa `put`/`delete`-rivejä:
//! poistetut ja korvatut rivit jäävät kuolleina riveinä lokiin, jolloin tiedosto
//! paisuu ja replay muuttuu O(n):ksi rivimäärässä. [`FileJournal::rewrite`]
//! korvaa **koko lokin** annetulla rivijoukolla **atomisesti**: rivit
//! kirjoitetaan ensin samaan hakemistoon luotuun tilapäistiedostoon (flush +
//! fsync), minkä jälkeen tilapäistiedosto **nimetään uudelleen** elävän tiedoston
//! päälle (`fs::rename`, atominen samalla levyllä). Jos prosessi kaatuu kesken
//! tiivistyksen, elävä tiedosto on yhä vanhassa (ehjässä) tilassaan — koskaan ei
//! synny puolikkaaksi kirjoitettua lokia. Kutsujan **vastuulla** on antaa
//! `rewrite`:lle tasan ne rivit, jotka kuvaavat halutun lopputilan (yleensä vain
//! elävät kirjaukset, kuolleet tombstonet pudotettuina).
//!
//! ## Tiivistys ilman TOCTOU-aukkoa — [`FileJournal::compact_with`]
//! [`FileJournal::rewrite`] korvaa lokin atomisesti levyä vastaan, mutta jos
//! kutsuja lukee tilan (replay) ENNEN `rewrite`:ä ja **vapauttaa lukon välissä**,
//! syntyy time-of-check-to-time-of-use-aukko: rinnakkainen append voi kirjoittaa
//! vanhaan tiedostoon juuri ennen kuin `rewrite` ylikirjoittaa sen
//! ennen-aukkoa-otetulla tilannekuvalla → append **katoaa hiljaisesti**.
//! [`FileJournal::compact_with`] sulkee aukon pitämällä **saman** file-lukon koko
//! luku→suodatus→swap-operaation ajan, jolloin appendit eivät mahdu väliin.
//! Kutsuja antaa `build`-sulkimen joka saa luetut rivit ja palauttaa
//! säilytettävät; itse swap on identtinen `rewrite`:n kanssa.
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
        // Poison-recovery sen sijaan että `unwrap()` paniikkaisi: jos jokin toinen
        // säie paniikkasi pitäessään lukkoa, file-kahva on silti validi (mitään ei
        // jätetä puolitiehen `read_all_entries`-polulla). `into_inner()` ottaa
        // kahvan haltuun ilman paniikkia → ei rikota error.rs:5 invarianttia
        // ("ei unwrap/expect/panic tuotantopolulla").
        let _file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Itse jäsennys ei käytä `self.file`-kahvaa vaan avaa tuoreen
        // lukukahvan polusta — `parse_entries_from_path` EI siis lukitse
        // `self.file`:ä uudelleen (jos lukitsisi, lukko olisi jo hallussa ja std
        // Mutex EI ole reentrantti → deadlock). Lukko pidetään tämän kutsun ajan
        // jotta luku on yhtenäinen samanaikaisten appendien suhteen.
        Self::parse_entries_from_path(&self.path)
    }

    /// Jäsentää **kaikki** journal-rivit annetusta polusta, sietäen vajaan
    /// viimeisen rivin (kaatumisen jälki). Palautuvat rivit ovat
    /// tiedostojärjestyksessä.
    ///
    /// ## Ei lukitusta — tarkoituksella
    /// Tämä apuri **ei** lukitse `self.file`-mutexia: se avaa oman tuoreen
    /// lukukahvan polusta. Syy on non-reentranttisuus: sekä `read_all_entries`
    /// että [`compact_with`](FileJournal::compact_with) lukitsevat `self.file`:n
    /// JO ennen tämän kutsumista, ja std [`Mutex`] **ei ole reentrantti** — jos
    /// tämä yrittäisi lukita lukon uudelleen saman säikeen sisältä, seurauksena
    /// olisi **deadlock**. Siksi jäsennys on eriytetty lukottomaan apuriin jonka
    /// molemmat lukon haltijat voivat kutsua turvallisesti.
    fn parse_entries_from_path(path: &Path) -> Result<Vec<JournalEntry>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        // Kerää (rivinumero, sisältö) jotta vajaa viimeinen rivi voidaan
        // tunnistaa luotettavasti.
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
        if let Some(last_byte) = last_byte_of(path)? {
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

    /// Korvaa **koko lokin** annetuilla riveillä atomisesti (tiivistys).
    ///
    /// Käyttötarkoitus: append-only-lokin päälle rakennettu tila (esim. odottavat
    /// hyväksynnät / jatkettavat vuorot) kerryttää kuolleita rivejä (poistot ja
    /// korvaukset) joita replay joutuu silti lukemaan. Tämä metodi kirjoittaa
    /// lokin uudelleen sisältämään **vain** annetut rivit — kutsuja antaa
    /// tyypillisesti vain elävät kirjaukset, jolloin kuolleet rivit katoavat ja
    /// tiedosto kutistuu.
    ///
    /// ## Atomisuus (ei koskaan turmele elävää tiedostoa)
    /// 1. Rivit kirjoitetaan **samaan hakemistoon** luotuun tilapäistiedostoon
    ///    (`<polku>.compact-<pid>-<aika>.tmp`).
    /// 2. Tilapäistiedosto flushataan ja **fsyncataan** ([`File::sync_all`]).
    /// 3. Tilapäistiedosto **nimetään uudelleen** elävän tiedoston päälle
    ///    ([`std::fs::rename`]) — sama-levyinen rename on atominen: lukija näkee
    ///    joko vanhan tai uuden tiedoston, ei koskaan puolikasta.
    /// 4. Sisäinen kirjoituskahva vaihdetaan osoittamaan uuteen (nimettyyn)
    ///    tiedostoon append-tilassa, jotta tulevat [`append`](Journal::append):it
    ///    jatkavat tiivistetyn lokin perään.
    ///
    /// Jos prosessi kaatuu **ennen** renamea, elävä tiedosto on koskematon (vanha
    /// ehjä tila säilyy) ja tilapäistiedosto jää orvoksi (harmiton; seuraava
    /// `rewrite` ylikirjoittaa oman uniikin nimensä). Jos kaatuu **renamen
    /// jälkeen**, uusi tiivistetty tiedosto on jo paikallaan ja ehjä. Kummassakaan
    /// tapauksessa eläviä rivejä ei katoa.
    ///
    /// Rivit kirjoitetaan annetussa järjestyksessä; [`StepId`]:t säilyvät
    /// sellaisinaan (kutsuja voi uudelleennumeroida ne ennen kutsua jos haluaa
    /// tiiviin 0..N-sekvenssin). Tyhjä `entries` tyhjentää lokin kokonaan.
    ///
    /// # Errors
    /// [`DurableError::Io`] jos tilapäistiedoston luonti, kirjoitus, fsync,
    /// rename tai uuden kahvan avaus epäonnistuu; [`DurableError::Serde`] jos
    /// jonkin rivin sarjallistus epäonnistuu. Virhetilanteessa elävä tiedosto
    /// jätetään entiselleen (rename tehdään vasta kun temp on ehjä levyllä).
    pub fn rewrite(&self, entries: &[JournalEntry]) -> Result<()> {
        // Lukko pidetään koko swapin ajan: append ei saa kirjoittaa vanhaan
        // kahvaan renamen ja kahvanvaihdon välissä.
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Atominen swap jaetussa apurissa (sama logiikka kuin `compact_with`:ssä).
        // `&mut *file` antaa apurille jo-hallussa-olevan lukon sisuksen, joten se
        // EI lukitse `self.file`:ä uudelleen (non-reentrantti std Mutex).
        self.atomic_swap_locked(&mut file, entries)
    }

    /// Tiivistää lokin **atomisesti appendeja vastaan**: lukitsee `self.file`:n
    /// **kerran**, lukee koko nykyisen lokin lukon ollessa hallussa, antaa rivit
    /// `build`-sulkimelle (joka palauttaa säilytettävät rivit), ja tekee saman
    /// atomisen temp + fsync + rename + kahvanvaihdon kuin
    /// [`rewrite`](FileJournal::rewrite) — **kaikki saman, yhä-hallussa-olevan
    /// lukon alla**. Palauttaa pudotettujen rivien määrän (luetut − säilytetyt,
    /// alarajattuna nollaan).
    ///
    /// ## Miksi atominen appendeja vastaan (TOCTOU-aukon sulkeminen)
    /// Aiempi tiivistys luki tilan ([`replay_all`](Journal::replay_all)), **vapautti
    /// lukon**, rakensi elävät rivit ja kutsui vasta sitten
    /// [`rewrite`](FileJournal::rewrite):ä (joka lukitsi uudelleen). Lukon
    /// vapautuksen ja rewriten välissä rinnakkainen append saattoi kirjoittaa
    /// **vanhaan** tiedostoon — ja rewrite ylikirjoitti sen
    /// ennen-aukkoa-otetulla tilannekuvalla, jolloin append **katosi
    /// hiljaisesti**. `compact_with` poistaa aukon: koska lukko pidetään lukemisen,
    /// rakentamisen JA swapin ajan, mikään append ei mahdu väliin — appendit
    /// joko valmistuvat ennen lukon ottoa (ja näkyvät `build`:n riveissä) tai
    /// jonottavat swapin jälkeiseen tiivistettyyn lokiin.
    ///
    /// ## Ei-reentranttisuus (miksi `build` ei saa kutsua takaisin)
    /// Lukko on jo hallussa kun `build` ajetaan, ja std [`Mutex`] **ei ole
    /// reentrantti**. Siksi tämä metodi EI kutsu `read_all_entries`:ä eikä
    /// [`rewrite`](FileJournal::rewrite):ä sisältään (molemmat lukitsisivat
    /// `self.file`:n uudelleen → **deadlock**). Sen sijaan se käyttää lukotonta
    /// jäsennysapuria `parse_entries_from_path` ja lukotonta swap-apuria
    /// `atomic_swap_locked` (yksityisiä apureita jotka eivät lukitse `self.file`:ä).
    /// `build`-suljin EI myöskään saa kutsua mitään tämän journalin lukitsevaa
    /// metodia (`append`, `replay_*`, `rewrite`, `compact_with`) — se johtaisi
    /// samaan deadlockiin. Sopimuksen mukaan `build` tekee vain puhdasta
    /// rivien suodatusta/uudelleennumerointia.
    ///
    /// ## Atomisuus levyä vastaan
    /// Sama tae kuin [`rewrite`](FileJournal::rewrite):llä: rivit kirjoitetaan ensin tilapäistiedostoon
    /// (flush + fsync), joka sitten nimetään atomisesti elävän tiedoston päälle.
    /// Jos prosessi kaatuu ennen renamea, elävä tiedosto on yhä ehjässä vanhassa
    /// tilassaan; jos kaatuu renamen jälkeen, uusi tiivistetty tiedosto on jo
    /// paikallaan. Eläviä rivejä ei katoa kummassakaan tapauksessa.
    ///
    /// # Errors
    /// [`DurableError::Io`] jos lukeminen, tilapäistiedoston kirjoitus, fsync,
    /// rename tai uuden kahvan avaus epäonnistuu; [`DurableError::Serde`] jos
    /// rivin sarjallistus epäonnistuu; tai `build`-sulkimen palauttama virhe
    /// sellaisenaan. Virhetilanteessa elävä tiedosto jätetään entiselleen.
    pub fn compact_with<F>(&self, build: F) -> Result<usize>
    where
        F: FnOnce(Vec<JournalEntry>) -> Result<Vec<JournalEntry>>,
    {
        // Lukko otetaan KERRAN ja pidetään koko luku→suodatus→swap-operaation
        // ajan. Tämä on koko TOCTOU-korjauksen ydin: appendit eivät mahdu väliin.
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Lue koko nykyinen loki lukottomalla apurilla (lukko on JO hallussa →
        // emme saa kutsua `read_all_entries`:ä joka lukitsisi uudelleen).
        let current = Self::parse_entries_from_path(&self.path)?;
        let read_count = current.len();

        // Kutsuja rakentaa säilytettävät rivit (suodattaa kuolleet, uudelleen-
        // numeroi StepId:t). Virhe palautetaan sellaisenaan, elävä tiedosto on
        // yhä koskematon (swapia ei ole vielä tehty).
        let kept = build(current)?;
        let kept_count = kept.len();

        // Atominen swap saman, yhä-hallussa-olevan lukon alla.
        self.atomic_swap_locked(&mut file, &kept)?;

        Ok(read_count.saturating_sub(kept_count))
    }

    /// Tekee atomisen temp + fsync + rename + kahvanvaihdon **olettaen että
    /// kutsuja pitää jo `self.file`-lukkoa** (`file` = kyseinen lukko-guard).
    ///
    /// Eriytetty jaettu apuri jotta [`rewrite`](FileJournal::rewrite) ja
    /// [`compact_with`](FileJournal::compact_with) tekevät täsmälleen saman
    /// swapin. **Ei lukitse `self.file`:ä uudelleen** — std [`Mutex`] ei ole
    /// reentrantti, joten lukko otetaan vain kerran kutsujassa ja annetaan tänne
    /// guardina. Swapin vaiheet:
    /// 1. Sarjallista kaikki rivit muistiin (jos serde kaatuu, levyä ei kosketa).
    /// 2. Kirjoita tilapäistiedostoon **samaan hakemistoon** (flush + fsync).
    /// 3. Nimeä tilapäistiedosto atomisesti elävän tiedoston päälle.
    /// 4. Vaihda kirjoituskahva osoittamaan uuteen tiedostoon append-tilassa.
    fn atomic_swap_locked(
        &self,
        file: &mut std::sync::MutexGuard<'_, File>,
        entries: &[JournalEntry],
    ) -> Result<()> {
        // 1: sarjallista KAIKKI rivit ennen kuin kosketaan levyyn.
        let mut buf = String::new();
        for entry in entries {
            let line = serde_json::to_string(entry)?;
            buf.push_str(&line);
            buf.push('\n');
        }

        // Tilapäistiedosto SAMAAN hakemistoon (rename on atominen vain saman
        // tiedostojärjestelmän sisällä). Uniikki nimi estää rinnakkaisten
        // tiivistysten törmäyksen.
        let tmp_path = self.compaction_tmp_path();

        // 2: kirjoita temp + flush + fsync.
        let write_result = (|| -> Result<()> {
            let mut tmp = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)?;
            tmp.write_all(buf.as_bytes())?;
            tmp.flush()?;
            tmp.sync_all()?;
            Ok(())
        })();
        if let Err(e) = write_result {
            // Temp jäi mahdollisesti vajaaksi — siivoa, elävä tiedosto koskematon.
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }

        // 3: atominen rename temp → elävä tiedosto.
        if let Err(e) = std::fs::rename(&tmp_path, &self.path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(DurableError::Io(e));
        }

        // 4: vaihda kirjoituskahva osoittamaan uuteen tiedostoon append-tilassa.
        let new_handle = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        **file = new_handle;

        // fsync hakemisto ei ole siirrettävä Windowsilla; rename + temp-fsync
        // antavat riittävän takeen (rename on atominen, temp-data on levyllä).
        Ok(())
    }

    /// Rakentaa uniikin tilapäistiedostopolun tiivistystä varten samaan
    /// hakemistoon kuin elävä loki (jotta rename on atominen).
    fn compaction_tmp_path(&self) -> PathBuf {
        use std::fmt::Write as _;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut name = self
            .path
            .file_name()
            .map_or_else(|| "journal".to_string(), |n| n.to_string_lossy().into_owned());
        // `write!` String:iin ei voi epäonnistua → tulos sivuutetaan tarkoituksella.
        let _ = write!(name, ".compact-{}-{nanos}.tmp", std::process::id());
        match self.path.parent() {
            Some(dir) => dir.join(name),
            None => PathBuf::from(name),
        }
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
        // Poison-recovery: jos lukon haltija paniikkasi, file-kahva on yhä validi
        // (append on atominen write_all + flush + fsync, ei osittaista tilaa joka
        // vaatisi mutex-poisonin kunnioittamista). `into_inner()` palauttaa kahvan
        // paniikkaamatta → noudattaa error.rs:5 invarianttia.
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    use crate::entry::EntryKind;
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

    /// REGRESSIO (error.rs:5 invariantti "ei unwrap/expect/panic tuotantopolulla"):
    /// kun toinen säie paniikkaa pitäessään file-mutexia, lukko myrkyttyy.
    /// `append` ja `read_all_entries`/`replay_all` käyttävät nyt
    /// `unwrap_or_else(|e| e.into_inner())` → ne TOIPUVAT myrkystä eivätkä
    /// paniikkaa. File-kahva pysyy validina, joten round-trip onnistuu.
    // ---- rewrite (compaction) ----

    #[test]
    fn rewrite_replaces_log_with_given_entries() {
        let tmp = TempPath::new("rewrite-basic");
        let j = FileJournal::open(tmp.path()).expect("open");
        // Aluksi viisi riviä.
        for i in 0..5 {
            j.append(JournalEntry::completed(StepId::new(i), "old", json!(i)))
                .expect("append");
        }
        assert_eq!(j.replay_all().expect("replay").len(), 5);

        // Tiivistä kahteen riviin.
        let kept = vec![
            JournalEntry::completed(StepId::ZERO, "keep-a", json!(1)),
            JournalEntry::completed(StepId::new(1), "keep-b", json!(2)),
        ];
        j.rewrite(&kept).expect("rewrite");

        let all = j.replay_all().expect("replay after rewrite");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("keep-a"));
        assert_eq!(all[1].step_name(), Some("keep-b"));
    }

    #[test]
    fn rewrite_result_survives_reopen() {
        let tmp = TempPath::new("rewrite-reopen");
        {
            let j = FileJournal::open(tmp.path()).expect("open 1");
            for i in 0..4 {
                j.append(JournalEntry::completed(StepId::new(i), "x", json!(i)))
                    .expect("append");
            }
            let kept = vec![JournalEntry::completed(StepId::ZERO, "only", json!(9))];
            j.rewrite(&kept).expect("rewrite");
        }
        // Restart: tiivistetty muoto säilyi levyllä.
        let j2 = FileJournal::open(tmp.path()).expect("open 2");
        let all = j2.replay_all().expect("replay");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].step_name(), Some("only"));
    }

    #[test]
    fn append_after_rewrite_continues_on_compacted_log() {
        let tmp = TempPath::new("rewrite-append");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "x", json!(i)))
                .expect("append");
        }
        j.rewrite(&[JournalEntry::completed(StepId::ZERO, "base", json!(0))])
            .expect("rewrite");
        // Append tiivistyksen jälkeen jatkaa puhtaalta rivirajalta.
        j.append(JournalEntry::completed(StepId::new(1), "next", json!(1)))
            .expect("append after rewrite");
        let all = j.replay_all().expect("replay");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("base"));
        assert_eq!(all[1].step_name(), Some("next"));
        // Yksikään fyysinen rivi ei sulauta kahta entryä.
        let contents = std::fs::read_to_string(j.path()).expect("read");
        assert!(
            !contents
                .lines()
                .any(|l| l.matches("\"step_id\"").count() >= 2),
            "no physical line may fuse two entries:\n{contents}"
        );
    }

    #[test]
    fn rewrite_to_empty_clears_log() {
        let tmp = TempPath::new("rewrite-empty");
        let j = FileJournal::open(tmp.path()).expect("open");
        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        j.rewrite(&[]).expect("rewrite empty");
        assert!(j.replay_all().expect("replay").is_empty());
        assert_eq!(std::fs::read_to_string(j.path()).expect("read").len(), 0);
    }

    #[test]
    fn rewrite_leaves_no_temp_file_behind() {
        let tmp = TempPath::new("rewrite-no-temp");
        let j = FileJournal::open(tmp.path()).expect("open");
        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        j.rewrite(&[JournalEntry::completed(StepId::ZERO, "b", json!(2))])
            .expect("rewrite");

        // TÄMÄN lokin temp-tiedostoa ei saa lojua hakemistossa (rename siirsi sen).
        // Rajataan skannaus tämän testin tiedostonimi-etuliitteeseen, jottei
        // rinnakkaisten testien lennossa olevat temp-tiedostot häiritse.
        let dir = j.path().parent().expect("parent dir");
        let own_name = j
            .path()
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .into_owned();
        let leftover: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&own_name) && n.contains(".compact-") && n.contains(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "temp files left behind: {leftover:?}");
    }

    /// Atomisuus: simuloidaan "keskeytys ennen renamea" jättämällä elävä tiedosto
    /// koskematta ja todistamalla että eläviä rivejä ei katoa. Koska oikeaa
    /// crashia ei voi laukaista deterministisesti, testi varmistaa invariantin:
    /// elävä tiedosto on aina ehjä rename-pohjaisella swapilla — ennen renamea
    /// sen sisältö on vanha kokonaisuus, ei koskaan puolikas.
    #[test]
    fn rewrite_is_atomic_live_file_never_half_written() {
        let tmp = TempPath::new("rewrite-atomic");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "live", json!(i)))
                .expect("append");
        }
        let before = std::fs::read_to_string(j.path()).expect("read before");

        // Tiivistä — atominen rename. Heti onnistumisen jälkeen tiedosto on
        // joko TÄYSIN vanha tai TÄYSIN uusi, ei sekoitus.
        let kept = vec![JournalEntry::completed(StepId::ZERO, "compacted", json!(42))];
        j.rewrite(&kept).expect("rewrite");
        let after = std::fs::read_to_string(j.path()).expect("read after");

        // Jokainen rivi on ehjä JSON (ei puolikasta sulautumaa renamesta).
        for line in after.lines().filter(|l| !l.trim().is_empty()) {
            serde_json::from_str::<JournalEntry>(line)
                .expect("every line after rewrite must be intact json");
        }
        assert_ne!(before, after, "rewrite must have changed contents");
        assert_eq!(j.replay_all().expect("replay").len(), 1);
    }

    // ---- compact_with (TOCTOU-aukon sulkeva atominen tiivistys) ----

    #[test]
    fn compact_with_filters_and_renumbers_under_single_lock() {
        let tmp = TempPath::new("compact-with-basic");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..5 {
            j.append(JournalEntry::completed(StepId::new(i), "old", json!(i)))
                .expect("append");
        }

        // Tiivistä: säilytä vain parilliset askeleet, uudelleennumeroi 0..N.
        let dropped = j
            .compact_with(|entries| {
                assert_eq!(entries.len(), 5, "build sees all on-disk rows");
                let mut kept = Vec::new();
                let mut step = StepId::ZERO;
                for e in entries.into_iter().filter(|e| {
                    matches!(&e.kind, EntryKind::StepCompleted { output, .. } if output.as_u64().is_some_and(|n| n % 2 == 0))
                }) {
                    kept.push(JournalEntry::completed(
                        step,
                        e.step_name().unwrap_or("kept").to_string(),
                        match &e.kind {
                            EntryKind::StepCompleted { output, .. } => output.clone(),
                            _ => json!(null),
                        },
                    ));
                    step = step.next();
                }
                Ok(kept)
            })
            .expect("compact_with");

        // 5 luettua − 3 säilytettyä (0,2,4) = 2 pudotettua.
        assert_eq!(dropped, 2);
        let all = j.replay_all().expect("replay");
        assert_eq!(all.len(), 3);
        // Uudelleennumeroitu tiiviiksi 0..N.
        assert_eq!(all[0].step_id, StepId::new(0));
        assert_eq!(all[1].step_id, StepId::new(1));
        assert_eq!(all[2].step_id, StepId::new(2));
    }

    /// TOCTOU-aukon sulkemisen TODISTE (regressio: append-during-compact ei katoa).
    ///
    /// Atomisuus seuraa yhden-lukon-pidosta: `compact_with` lukitsee `self.file`:n
    /// kerran ja pitää sen luku→suodatus→swap-ajan. Tämä testi todistaa kaksi
    /// havaittavaa seurausta:
    /// 1. `build`-suljin näkee **tasan** ne rivit jotka olivat levyllä lukon
    ///    ottohetkellä (ei enempää, ei vähempää) — luku on yhtenäinen.
    /// 2. `compact_with`:n PALUUN JÄLKEEN tehty append laskeutuu tiivistettyjen
    ///    rivien PERÄÄN — ei vanhaan tiedostoon joka tuhoutuisi swapissa.
    ///
    /// Koska lukko pidetään koko ajan, rinnakkainen append voi valmistua vain
    /// ENNEN lukon ottoa (jolloin se näkyy `build`:n riveissä) tai swapin jälkeen
    /// (jolloin se laskeutuu tiivistetyn lokin perään) — ei koskaan väliin
    /// katoamaan. Tässä ei tarvita oikeaa rinnakkaisuutta: invariantti seuraa
    /// rakenteesta, ja deterministinen testi on luotettavampi kuin race.
    #[test]
    fn compact_with_holds_lock_so_post_compact_append_lands_after_compacted_rows() {
        let tmp = TempPath::new("compact-with-toctou");
        let j = FileJournal::open(tmp.path()).expect("open");
        // Kolme riviä levylle ENNEN tiivistystä.
        for i in 0..3 {
            j.append(JournalEntry::completed(StepId::new(i), "pre", json!(i)))
                .expect("append pre");
        }

        // Tiivistä: build näkee TASAN nuo kolme riviä (yhtenäinen luku lukon alla).
        let dropped = j
            .compact_with(|entries| {
                assert_eq!(
                    entries.len(),
                    3,
                    "build must observe exactly the rows on disk at lock-acquire"
                );
                for (i, e) in entries.iter().enumerate() {
                    assert_eq!(e.step_name(), Some("pre"));
                    assert_eq!(e.step_id, StepId::new(i as u64));
                }
                // Säilytä vain yksi (uudelleennumeroitu) elävä rivi.
                Ok(vec![JournalEntry::completed(StepId::ZERO, "compacted", json!(99))])
            })
            .expect("compact_with");
        assert_eq!(dropped, 2, "3 read − 1 kept = 2 dropped");

        // Append PALUUN JÄLKEEN laskeutuu tiivistettyjen rivien PERÄÄN.
        j.append(JournalEntry::completed(StepId::new(1), "post", json!(100)))
            .expect("append post must land after compacted rows");

        let all = j.replay_all().expect("replay");
        assert_eq!(all.len(), 2, "1 compacted + 1 post-append = 2");
        assert_eq!(all[0].step_name(), Some("compacted"));
        assert_eq!(all[1].step_name(), Some("post"));

        // Yksikään fyysinen rivi ei sulauta kahta entryä (swap jätti puhtaat rajat).
        let contents = std::fs::read_to_string(j.path()).expect("read");
        assert!(
            !contents
                .lines()
                .any(|l| l.matches("\"step_id\"").count() >= 2),
            "no physical line may fuse two entries:\n{contents}"
        );
    }

    /// EI-DEADLOCK: `compact_with` lukitsee `self.file`:n vain kerran (ei
    /// reentranttia uudelleenlukitusta), joten kutsu valmistuu, ja heti perään
    /// tehty append + replay onnistuvat samalla säikeellä. Jos jokin
    /// sisäpolku lukitsisi `self.file`:n uudelleen, tämä testi jumiutuisi
    /// (timeout) tai paniikkaisi.
    #[test]
    fn compact_with_does_not_deadlock_then_append_then_replay() {
        let tmp = TempPath::new("compact-with-no-deadlock");
        let j = FileJournal::open(tmp.path()).expect("open");
        for i in 0..4 {
            j.append(JournalEntry::completed(StepId::new(i), "x", json!(i)))
                .expect("append");
        }

        // compact_with valmistuu (ei deadlock samalla säikeellä).
        j.compact_with(|entries| {
            // Suodata pois pariton output → säilytä 0 ja 2.
            let mut kept = Vec::new();
            let mut step = StepId::ZERO;
            for e in entries {
                let keep = matches!(&e.kind, EntryKind::StepCompleted { output, .. } if output.as_u64().is_some_and(|n| n % 2 == 0));
                if keep {
                    let out = match &e.kind {
                        EntryKind::StepCompleted { output, .. } => output.clone(),
                        _ => json!(null),
                    };
                    kept.push(JournalEntry::completed(step, "live", out));
                    step = step.next();
                }
            }
            Ok(kept)
        })
        .expect("compact_with completes without deadlock");

        // HETI perään append (lukitsee self.file uudelleen — onnistuu koska
        // compact_with vapautti lukkonsa palatessaan).
        j.append(JournalEntry::completed(StepId::new(2), "after", json!(7)))
            .expect("append after compact_with");

        // Ja replay (lukitsee self.file vielä kerran) — onnistuu.
        let all = j.replay_all().expect("replay after compact_with");
        assert_eq!(all.len(), 3, "2 live (0,2) + 1 after-append");
        assert_eq!(all[2].step_name(), Some("after"));
    }

    #[test]
    fn compact_with_propagates_build_error_and_leaves_live_file_intact() {
        let tmp = TempPath::new("compact-with-build-err");
        let j = FileJournal::open(tmp.path()).expect("open");
        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append");
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append");

        // build palauttaa virheen → compact_with palauttaa sen sellaisenaan,
        // eikä elävää tiedostoa muuteta (swapia ei tehdä).
        let err = j
            .compact_with(|_entries| {
                Err(DurableError::step_failed("build", "intentional build failure"))
            })
            .expect_err("build error must propagate");
        match err {
            DurableError::StepFailed { step, .. } => assert_eq!(step, "build"),
            other => panic!("unexpected error: {other:?}"),
        }

        // Elävä tiedosto koskematon: molemmat alkuperäiset rivit yhä luettavissa.
        let all = j.replay_all().expect("replay after failed compact");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
    }

    #[test]
    fn compact_with_on_empty_log_is_noop() {
        let tmp = TempPath::new("compact-with-empty");
        let j = FileJournal::open(tmp.path()).expect("open");
        let dropped = j
            .compact_with(|entries| {
                assert!(entries.is_empty(), "empty log yields no rows");
                Ok(entries)
            })
            .expect("compact_with");
        assert_eq!(dropped, 0);
        assert!(j.replay_all().expect("replay").is_empty());
        assert_eq!(std::fs::read_to_string(j.path()).expect("read").len(), 0);
    }

    #[test]
    fn append_and_replay_recover_from_poisoned_mutex() {
        use std::sync::Arc;

        let tmp = TempPath::new("poison-recovery");
        let j = Arc::new(FileJournal::open(tmp.path()).expect("open"));
        // Yksi ehjä askel ennen myrkytystä.
        j.append(JournalEntry::completed(StepId::ZERO, "a", json!(1)))
            .expect("append a");

        // Myrkytä mutex: paniikkaa toisessa säikeessä lukon ollessa hallussa.
        let poisoner = Arc::clone(&j);
        let handle = std::thread::spawn(move || {
            let _guard = poisoner.file.lock().expect("acquire lock to poison");
            panic!("intentional panic to poison the file mutex");
        });
        assert!(
            handle.join().is_err(),
            "poisoning thread must have panicked"
        );

        // append TOIPUU myrkystä — ei paniikkaa, palauttaa Ok.
        j.append(JournalEntry::completed(StepId::new(1), "b", json!(2)))
            .expect("append must recover from poisoned mutex");

        // replay_all (→ read_all_entries) TOIPUU myös ja näkee molemmat askeleet.
        let all = j
            .replay_all()
            .expect("replay must recover from poisoned mutex");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].step_name(), Some("a"));
        assert_eq!(all[1].step_name(), Some("b"));
    }
}
