//! Deterministinen uudelleenajo (LOOP-idea).
//!
//! Paperi **LOOP** (2605.14237) esittää: tallenna suoritus *kerran*
//! järjestettynä tapahtumalokina, ja toista se sen jälkeen deterministisesti
//! pelkästä lokista — ilman kelloa, verkkoa tai mitään ulkoista syötettä.
//! Koska kaikki ei-deterministiset syötteet (luetut tavut, polttoaineen
//! kulutus, lopputulos) on *tallennettu* tapahtumiin, uudelleenajo ei tarvitse
//! alkuperäistä backendia eikä ympäristöä. Tämä on edellytys luotettavalle
//! debuggaukselle ja auditoinnille (sama suoritus voidaan tutkia bitintarkasti
//! jälkikäteen) ja se leikkaa token/laskenta-kustannuksen murto-osaan.
//!
//! ## Mitä tallennetaan
//! [`ExecutionTrace`] on järjestetty [`TraceEvent`]-jono. Tapahtumat kuvaavat
//! determinismin kannalta olennaiset *havainnot*:
//! - [`TraceEvent::Started`] — suoritus alkoi (backend + koodin koko).
//! - [`TraceEvent::Output`] — koodi tuotti tavulohkon.
//! - [`TraceEvent::FuelConsumed`] — polttoainetta kului `amount` yksikköä.
//! - [`TraceEvent::Finished`] — suoritus päättyi (onnistui / epäonnistui).
//!
//! ## Uudelleenajo
//! [`replay`] kävelee tapahtumat läpi ja kokoaa [`Outcome`]:n täysin
//! deterministisesti: se *ei* aja WASM-koodia, *ei* lue kelloa eikä verkkoa. Saman
//! [`ExecutionTrace`]:n uudelleenajo tuottaa aina identtisen [`Outcome`]:n.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_sandbox::{ExecutionTrace, TraceEvent, replay};
//!
//! let trace = ExecutionTrace::new(vec![
//!     TraceEvent::started("noop", 4),
//!     TraceEvent::output(b"hello".to_vec()),
//!     TraceEvent::fuel_consumed(7),
//!     TraceEvent::finished(true),
//! ]);
//!
//! let a = replay(&trace).expect("replay ok");
//! let b = replay(&trace).expect("replay ok again");
//! assert_eq!(a, b); // determinismi
//! assert_eq!(a.output, b"hello");
//! assert_eq!(a.fuel_consumed, 7);
//! assert!(a.success);
//! ```

use serde::{Deserialize, Serialize};

use crate::error::{Result, SandboxError};
use crate::sandbox::SandboxOutput;

/// Yksittäinen tallennettu tapahtuma suorituksen aikana.
///
/// Tapahtumat ovat sarjallistuvia ja determinismin kannalta täydellisiä:
/// uudelleenajo ei tarvitse mitään tapahtumien ulkopuolelta.
///
/// `#[non_exhaustive]` jotta uusia tapahtumatyyppejä voi lisätä rikkomatta
/// downstream-koodia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event")]
#[non_exhaustive]
pub enum TraceEvent {
    /// Suoritus alkoi.
    Started {
        /// Backendin tunniste joka tuotti tämän lokin.
        backend: String,
        /// Ajetun koodin koko tavuina.
        code_len: usize,
    },

    /// Koodi tuotti tulostavuja. Useita lohkoja voi esiintyä; ne ketjutetaan
    /// uudelleenajossa esiintymisjärjestyksessä.
    Output {
        /// Tuotettu tavulohko.
        bytes: Vec<u8>,
    },

    /// Polttoainetta kului. Useat merkinnät lasketaan yhteen.
    FuelConsumed {
        /// Kulutettu määrä tässä askeleessa.
        amount: u64,
    },

    /// Suoritus päättyi.
    Finished {
        /// Päättyikö suoritus onnistuneesti.
        success: bool,
    },
}

impl TraceEvent {
    /// [`TraceEvent::Started`]-tapahtuma.
    pub fn started(backend: impl Into<String>, code_len: usize) -> Self {
        Self::Started {
            backend: backend.into(),
            code_len,
        }
    }

    /// [`TraceEvent::Output`]-tapahtuma.
    pub fn output(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Output {
            bytes: bytes.into(),
        }
    }

    /// [`TraceEvent::FuelConsumed`]-tapahtuma.
    #[must_use]
    pub const fn fuel_consumed(amount: u64) -> Self {
        Self::FuelConsumed { amount }
    }

    /// [`TraceEvent::Finished`]-tapahtuma.
    #[must_use]
    pub const fn finished(success: bool) -> Self {
        Self::Finished { success }
    }
}

/// Järjestetty, sarjallistuva loki yhdestä sandbox-suorituksesta.
///
/// Tämä on LOOP-mekanismin tallenne: kerran kirjattuna se voidaan toistaa
/// deterministisesti [`replay`]:n kautta ilman alkuperäistä backendia.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionTrace {
    events: Vec<TraceEvent>,
}

impl ExecutionTrace {
    /// Rakentaa lokin valmiista tapahtumajonosta.
    #[must_use]
    pub fn new(events: impl Into<Vec<TraceEvent>>) -> Self {
        Self {
            events: events.into(),
        }
    }

    /// Tyhjä loki, johon tapahtumia lisätään [`push`](ExecutionTrace::push):lla.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Lisää tapahtuman lokin loppuun (append-only).
    pub fn push(&mut self, event: TraceEvent) {
        self.events.push(event);
    }

    /// Tapahtumat lisäysjärjestyksessä.
    #[must_use]
    pub fn events(&self) -> &[TraceEvent] {
        &self.events
    }

    /// Tapahtumien lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Onko loki tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Uudelleenajon lopputulos.
///
/// Sarjallistuva tiivistelmä siitä mitä suoritus tuotti: tulostavut, kulutettu
/// polttoaine ja onnistuiko se. [`From<Outcome>`] tuottaa [`SandboxOutput`]:n
/// jotta uudelleenajo voidaan verrata alkuperäiseen suoritustulokseen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// Yhdistetyt tulostavut esiintymisjärjestyksessä.
    pub output: Vec<u8>,
    /// Yhteenlaskettu kulutettu polttoaine.
    pub fuel_consumed: u64,
    /// Onnistuiko suoritus.
    pub success: bool,
}

impl Outcome {
    /// Rakentaa lopputuloksen sen osista.
    #[must_use]
    pub fn new(output: impl Into<Vec<u8>>, fuel_consumed: u64, success: bool) -> Self {
        Self {
            output: output.into(),
            fuel_consumed,
            success,
        }
    }
}

impl From<Outcome> for SandboxOutput {
    /// Onnistunut/epäonnistunut [`Outcome`] kartoittuu tavuihin ja kulutukseen
    /// jotka [`SandboxOutput`] kantaa (success-lippu ei kuulu outputtiin —
    /// epäonnistuminen ilmaistaan tyypillisesti virheellä ylemmällä tasolla).
    fn from(outcome: Outcome) -> Self {
        SandboxOutput::new(outcome.output, outcome.fuel_consumed)
    }
}

/// Toistaa tallennetun [`ExecutionTrace`]:n deterministisesti.
///
/// Kävelee tapahtumat läpi ja kokoaa [`Outcome`]:n **pelkästä lokista** — ei
/// aja WASM-koodia, ei lue kelloa eikä verkkoa. Saman lokin uudelleenajo tuottaa
/// aina identtisen tuloksen (LOOP-takuu).
///
/// Determinismin vuoksi loki validoidaan rakenteellisesti: sen on alettava
/// [`TraceEvent::Started`]:lla ja loputtava [`TraceEvent::Finished`]:iin, ja
/// kumpaakaan ei saa esiintyä kahdesti. Näin epätäydellinen tai vioittunut
/// loki havaitaan eikä se tuota harhaanjohtavaa "onnistunutta" tulosta.
///
/// # Errors
/// [`SandboxError::Execution`] jos loki on rakenteellisesti kelvoton (tyhjä,
/// ei ala `Started`:lla, ei pääty `Finished`:iin, tai sisältää toisteisia
/// elinkaaritapahtumia).
pub fn replay(trace: &ExecutionTrace) -> Result<Outcome> {
    let events = trace.events();
    if events.is_empty() {
        return Err(SandboxError::execution("cannot replay an empty trace"));
    }

    let mut started = false;
    let mut finished = false;
    let mut output: Vec<u8> = Vec::new();
    let mut fuel_consumed: u64 = 0;
    let mut success = false;

    for event in events {
        match event {
            TraceEvent::Started { .. } => {
                if started {
                    return Err(SandboxError::execution(
                        "trace contains more than one Started event",
                    ));
                }
                started = true;
            }
            TraceEvent::Output { bytes } => {
                if !started || finished {
                    return Err(SandboxError::execution(
                        "Output event outside the Started..Finished window",
                    ));
                }
                output.extend_from_slice(bytes);
            }
            TraceEvent::FuelConsumed { amount } => {
                if !started || finished {
                    return Err(SandboxError::execution(
                        "FuelConsumed event outside the Started..Finished window",
                    ));
                }
                // Saturoiva yhteenlasku: vioittunut loki ei panikoi ylivuodosta.
                fuel_consumed = fuel_consumed.saturating_add(*amount);
            }
            TraceEvent::Finished { success: s } => {
                if !started {
                    return Err(SandboxError::execution(
                        "Finished event before any Started event",
                    ));
                }
                if finished {
                    return Err(SandboxError::execution(
                        "trace contains more than one Finished event",
                    ));
                }
                finished = true;
                success = *s;
            }
        }
    }

    if !finished {
        return Err(SandboxError::execution(
            "trace ended without a Finished event",
        ));
    }

    Ok(Outcome {
        output,
        fuel_consumed,
        success,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_trace() -> ExecutionTrace {
        ExecutionTrace::new(vec![
            TraceEvent::started("noop", 4),
            TraceEvent::output(b"hello ".to_vec()),
            TraceEvent::output(b"world".to_vec()),
            TraceEvent::fuel_consumed(10),
            TraceEvent::fuel_consumed(5),
            TraceEvent::finished(true),
        ])
    }

    #[test]
    fn replay_assembles_outcome_from_trace() {
        let outcome = replay(&happy_trace()).expect("replay ok");
        assert_eq!(outcome.output, b"hello world");
        assert_eq!(outcome.fuel_consumed, 15);
        assert!(outcome.success);
    }

    #[test]
    fn replay_is_deterministic() {
        let trace = happy_trace();
        let first = replay(&trace).expect("first ok");
        let second = replay(&trace).expect("second ok");
        let third = replay(&trace).expect("third ok");
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn replay_preserves_failure_flag() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::fuel_consumed(3),
            TraceEvent::finished(false),
        ]);
        let outcome = replay(&trace).expect("replay ok");
        assert!(!outcome.success);
        assert_eq!(outcome.fuel_consumed, 3);
        assert!(outcome.output.is_empty());
    }

    #[test]
    fn replay_rejects_empty_trace() {
        let err = replay(&ExecutionTrace::empty()).expect_err("empty must fail");
        assert!(err.to_string().contains("empty trace"));
    }

    #[test]
    fn replay_rejects_missing_finished() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::output(b"x".to_vec()),
        ]);
        let err = replay(&trace).expect_err("missing finished must fail");
        assert!(err.to_string().contains("without a Finished"));
    }

    #[test]
    fn replay_rejects_output_before_started() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::output(b"x".to_vec()),
            TraceEvent::started("noop", 1),
            TraceEvent::finished(true),
        ]);
        let err = replay(&trace).expect_err("output before start must fail");
        assert!(err.to_string().contains("outside the Started"));
    }

    #[test]
    fn replay_rejects_double_finished() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::finished(true),
            TraceEvent::finished(true),
        ]);
        let err = replay(&trace).expect_err("double finished must fail");
        assert!(err.to_string().contains("more than one Finished"));
    }

    #[test]
    fn replay_rejects_double_started() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::started("noop", 1),
            TraceEvent::finished(true),
        ]);
        let err = replay(&trace).expect_err("double started must fail");
        assert!(err.to_string().contains("more than one Started"));
    }

    #[test]
    fn replay_fuel_saturates_without_overflow() {
        let trace = ExecutionTrace::new(vec![
            TraceEvent::started("noop", 1),
            TraceEvent::fuel_consumed(u64::MAX),
            TraceEvent::fuel_consumed(10),
            TraceEvent::finished(true),
        ]);
        let outcome = replay(&trace).expect("replay ok");
        assert_eq!(outcome.fuel_consumed, u64::MAX);
    }

    #[test]
    fn trace_push_appends_in_order() {
        let mut trace = ExecutionTrace::empty();
        trace.push(TraceEvent::started("noop", 2));
        trace.push(TraceEvent::finished(true));
        assert_eq!(trace.len(), 2);
        assert!(!trace.is_empty());
        assert!(matches!(trace.events()[0], TraceEvent::Started { .. }));
    }

    #[test]
    fn trace_serde_roundtrip() {
        let trace = happy_trace();
        let json = serde_json::to_string(&trace).expect("serialize");
        let back: ExecutionTrace = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(trace, back);
        // Uudelleenajon tulos säilyy serde-kierroksen yli.
        assert_eq!(replay(&trace).unwrap(), replay(&back).unwrap());
    }

    #[test]
    fn outcome_converts_to_sandbox_output() {
        let outcome = Outcome::new(b"data".to_vec(), 42, true);
        let out: SandboxOutput = outcome.into();
        assert_eq!(out.output, b"data");
        assert_eq!(out.fuel_consumed, 42);
    }
}
