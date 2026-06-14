//! Kevyt mittarirekisteri ([`MetricsRegistry`]) ja sen mittarityypit
//! ([`Counter`], [`Gauge`], [`Histogram`]).
//!
//! Tämä moduuli toteuttaa **käsin kirjoitetun Prometheus-tekstiviennin** ilman
//! raskaita `metrics`-/`opentelemetry`-pinoja (sanat tarkoituksella ilman
//! linkkiä) — `FamilyClaw` arvostaa pieniä
//! (2–8 MB) binäärejä. Mittarit ovat atomisia (`AtomicU64`/`AtomicI64`) ja
//! rekisteri jakaa kahvat `Arc`:n kautta, joten useat säikeet voivat päivittää
//! samaa mittaria lukkojen kanssa kilpailematta päivityspolulla.
//!
//! ## Periaatteet
//! - **Idempotentit kahvat.** `counter(name)`/`gauge(name)`/`histogram(name)`
//!   palauttavat *saman* kahvan samalle nimelle (get-or-create). Kahva on
//!   `Arc`-jaettu, joten klooni näkee samat luvut.
//! - **Deterministinen vienti.** [`MetricsRegistry::prometheus_export`]
//!   järjestää mittarit nimen mukaan, joten tuloste on vakaa (helppo
//!   golden-string-testata).
//! - **Ei lukkoja kuumalla polulla.** Vain rekisterin nimi→kahva-haku ottaa
//!   lukon; itse inkrementit ovat lukkovapaita atomeja.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Monotonisesti kasvava laskuri.
///
/// Sopii kumulatiivisille tapahtumamäärille (esim. luodut tehtävät,
/// LLM-kutsut). Arvo on `u64` ja päivitykset ovat atomisia.
#[derive(Debug, Clone, Default)]
pub struct Counter {
    value: Arc<AtomicU64>,
}

impl Counter {
    /// Luo uuden laskurin arvolla `0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Kasvattaa laskuria yhdellä.
    pub fn inc(&self) {
        self.inc_by(1);
    }

    /// Kasvattaa laskuria annetulla määrällä.
    pub fn inc_by(&self, delta: u64) {
        self.value.fetch_add(delta, Ordering::Relaxed);
    }

    /// Palauttaa laskurin nykyarvon.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// Vapaasti kasvava ja laskeva mittari (gauge).
///
/// Sopii hetkellisille arvoille jotka voivat nousta ja laskea (esim. online
/// olevien agenttien määrä). Arvo on etumerkillinen `i64`.
#[derive(Debug, Clone, Default)]
pub struct Gauge {
    value: Arc<AtomicI64>,
}

impl Gauge {
    /// Luo uuden gaugen arvolla `0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asettaa gaugen arvon.
    pub fn set(&self, value: i64) {
        self.value.store(value, Ordering::Relaxed);
    }

    /// Lisää gaugeen annetun (positiivisen) määrän.
    pub fn add(&self, delta: i64) {
        self.value.fetch_add(delta, Ordering::Relaxed);
    }

    /// Vähentää gaugesta annetun määrän.
    pub fn sub(&self, delta: i64) {
        self.value.fetch_sub(delta, Ordering::Relaxed);
    }

    /// Palauttaa gaugen nykyarvon.
    #[must_use]
    pub fn get(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// Histogrammin kiinteät yläraja-ämpärit (`le`, "less than or equal").
///
/// Arvot ovat sekunteja (Prometheus-konventio kestoille), mutta histogrammia
/// voi käyttää mille tahansa ei-negatiiviselle suureelle.
const DEFAULT_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Histogrammi kiinteillä ämpäreillä.
///
/// Tallentaa kumulatiivisen `le`-ämpärijakauman, havaintojen summan ja
/// kokonaismäärän — kaikki Prometheus-yhteensopivassa muodossa. Ämpärirajat
/// lukitaan luontihetkellä, joten rajat ovat vakaat koko mittarin eliniän.
#[derive(Debug, Clone)]
pub struct Histogram {
    /// Ämpärien ylärajat nousevassa järjestyksessä (jaettu, muuttumaton).
    bounds: Arc<[f64]>,
    /// Per-ämpäri-laskurit (ei-kumulatiiviset; vienti kumuloi ne).
    counts: Arc<[AtomicU64]>,
    /// Havaintojen summa millihavaintoina skaalattuna (ks. [`Histogram::sum`]).
    sum_milli: Arc<AtomicU64>,
    /// Havaintojen kokonaismäärä.
    count: Arc<AtomicU64>,
}

/// Summan kiinteäpistemuunnoksen skaala (3 desimaalia).
///
/// `f64`-summaa ei voi tallentaa atomisesti turvallisesti ilman lukkoa, joten
/// summa pidetään kokonaislukuna (`arvo * 1000`). Tämä riittää Prometheus-
/// kestoille (millisekuntiresoluutio) ja tekee summasta deterministisen.
const SUM_SCALE: f64 = 1000.0;

/// `u64::MAX` lähimpänä `f64`-arvona (≈ 1.8446744e19), käytetään
/// summan yläleikkaukseen ilman lossy-castia kuumalla polulla.
const U64_MAX_AS_F64: f64 = 18_446_744_073_709_551_615.0;

impl Histogram {
    /// Luo histogrammin oletusämpäreillä ([`DEFAULT_BUCKETS`]).
    #[must_use]
    pub fn new() -> Self {
        Self::with_buckets(&DEFAULT_BUCKETS)
    }

    /// Luo histogrammin annetuilla ämpärirajoilla.
    ///
    /// Rajat siivotaan: ei-äärelliset ja ei-positiiviset poistetaan, loput
    /// järjestetään nousevasti ja duplikaatit poistetaan. Jos jäljelle ei jää
    /// yhtään rajaa, käytetään yhtä `+Inf`-ekvivalenttia ämpäriä (kaikki
    /// havainnot menevät `_count`-summaan).
    #[must_use]
    pub fn with_buckets(bounds: &[f64]) -> Self {
        let mut clean: Vec<f64> = bounds
            .iter()
            .copied()
            .filter(|b| b.is_finite() && *b > 0.0)
            .collect();
        clean.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        clean.dedup();
        let n = clean.len();
        let counts: Vec<AtomicU64> = (0..n).map(|_| AtomicU64::new(0)).collect();
        Self {
            bounds: Arc::from(clean.into_boxed_slice()),
            counts: Arc::from(counts.into_boxed_slice()),
            sum_milli: Arc::new(AtomicU64::new(0)),
            count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Kirjaa yhden havainnon.
    ///
    /// Ei-negatiivinen `value` lisätään pienimpään ämpäriin jonka yläraja se
    /// alittaa tai johon se osuu, kasvattaa kokonaismäärää ja summaa.
    /// Negatiiviset ja ei-äärelliset arvot ohitetaan hiljaisesti
    /// (Prometheus-histogrammi ei tue niitä).
    pub fn observe(&self, value: f64) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        for (i, bound) in self.bounds.iter().enumerate() {
            if value <= *bound {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        // Skaalattu kiinteäpistesumma (millit). Pyöristetään lähimpään.
        let scaled = (value * SUM_SCALE).round();
        // Rajaa ettei kaadu cast-aliasointiin valtavilla arvoilla.
        // `U64_MAX_AS_F64` on `u64::MAX` lähimpänä `f64`-arvona (tarkka cast
        // ei ole mahdollinen, joten käytämme vakiota välttääksemme lossy-castin
        // kuumalla polulla).
        let scaled = scaled.clamp(0.0, U64_MAX_AS_F64);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let milli = scaled as u64;
        self.sum_milli.fetch_add(milli, Ordering::Relaxed);
    }

    /// Havaintojen kokonaismäärä (`_count`).
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Havaintojen summa (`_sum`), millihavainnoista palautettuna `f64`:ksi.
    #[must_use]
    pub fn sum(&self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let raw = self.sum_milli.load(Ordering::Relaxed) as f64;
        raw / SUM_SCALE
    }

    /// Palauttaa kumulatiiviset `le`-ämpärit pareina `(yläraja, kumulatiivinen
    /// määrä)`. Viimeinen pari on aina `(+Inf, kokonaismäärä)`.
    #[must_use]
    pub fn cumulative_buckets(&self) -> Vec<(BucketBound, u64)> {
        let mut out = Vec::with_capacity(self.bounds.len() + 1);
        for (i, bound) in self.bounds.iter().enumerate() {
            let c = self.counts[i].load(Ordering::Relaxed);
            out.push((BucketBound::Le(*bound), c));
        }
        out.push((BucketBound::PosInf, self.count()));
        out
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

/// Histogrammiämpärin yläraja Prometheus-vientiä varten.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BucketBound {
    /// Äärellinen yläraja (`le="<arvo>"`).
    Le(f64),
    /// Positiivinen ääretön (`le="+Inf"`).
    PosInf,
}

impl BucketBound {
    /// Muotoilee `le`-arvon Prometheus-merkkijonoksi.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            BucketBound::Le(v) => format_float(v),
            BucketBound::PosInf => "+Inf".to_string(),
        }
    }
}

/// Mittarin tyyppi rekisterin sisällä.
#[derive(Debug, Clone)]
enum Metric {
    Counter(Counter),
    Gauge(Gauge),
    Histogram(Histogram),
}

/// Säieturvallinen mittarirekisteri.
///
/// Pitää nimettyjä mittareita ([`Counter`], [`Gauge`], [`Histogram`]) ja
/// vie ne deterministisessä Prometheus-tekstiformaatissa. Rekisteri on
/// `Clone` ja jakaa tilansa `Arc`:n kautta — kaikki kloonit näkevät samat
/// mittarit.
#[derive(Debug, Clone, Default)]
pub struct MetricsRegistry {
    inner: Arc<RwLock<BTreeMap<String, Metric>>>,
}

impl MetricsRegistry {
    /// Luo tyhjän rekisterin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Luo rekisterin jossa on monen agentin laivuetta varten esinimetyt
    /// laskurit ja gaugiet (ks. moduulin dokumentaatio).
    ///
    /// Tämä takaa että vienti sisältää nämä sarjat alusta asti (arvolla `0`),
    /// joten dashboardit eivät "katoa" ennen ensimmäistä tapahtumaa.
    #[must_use]
    pub fn with_fleet_defaults() -> Self {
        let reg = Self::new();
        for name in FLEET_COUNTERS {
            let _ = reg.counter(name);
        }
        let _ = reg.gauge(GAUGE_AGENTS_ONLINE);
        reg
    }

    /// Hakee tai luo nimetyn laskurin (idempotentti).
    ///
    /// Sama nimi palauttaa aina saman kahvan. Jos nimi on jo varattu eri
    /// mittarityypille, palautetaan *uusi irrallinen* laskuri jota ei
    /// rekisteröidä (tyyppi-ristiriidan turvallinen ohitus).
    #[must_use]
    pub fn counter(&self, name: &str) -> Counter {
        let mut guard = match self.inner.write() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        match guard.get(name) {
            Some(Metric::Counter(c)) => c.clone(),
            Some(_) => Counter::new(),
            None => {
                let c = Counter::new();
                guard.insert(name.to_string(), Metric::Counter(c.clone()));
                c
            }
        }
    }

    /// Hakee tai luo nimetyn gaugen (idempotentti).
    #[must_use]
    pub fn gauge(&self, name: &str) -> Gauge {
        let mut guard = match self.inner.write() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        match guard.get(name) {
            Some(Metric::Gauge(g)) => g.clone(),
            Some(_) => Gauge::new(),
            None => {
                let g = Gauge::new();
                guard.insert(name.to_string(), Metric::Gauge(g.clone()));
                g
            }
        }
    }

    /// Hakee tai luo nimetyn histogrammin oletusämpäreillä (idempotentti).
    #[must_use]
    pub fn histogram(&self, name: &str) -> Histogram {
        self.histogram_with_buckets(name, &DEFAULT_BUCKETS)
    }

    /// Hakee tai luo nimetyn histogrammin annetuilla ämpäreillä.
    ///
    /// Jos histogrammi luotiin jo (millä tahansa ämpäreillä), palautetaan
    /// olemassa oleva kahva — `buckets` huomioidaan vain ensiluonnissa.
    #[must_use]
    pub fn histogram_with_buckets(&self, name: &str, buckets: &[f64]) -> Histogram {
        let mut guard = match self.inner.write() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        match guard.get(name) {
            Some(Metric::Histogram(h)) => h.clone(),
            Some(_) => Histogram::with_buckets(buckets),
            None => {
                let h = Histogram::with_buckets(buckets);
                guard.insert(name.to_string(), Metric::Histogram(h.clone()));
                h
            }
        }
    }

    /// Rekisteröityjen mittareiden lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        let guard = match self.inner.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.len()
    }

    /// Onko rekisteri tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Vie kaikki mittarit deterministisessä Prometheus-tekstiformaatissa.
    ///
    /// - Mittarit järjestetään nimen mukaan (`BTreeMap` takaa tämän).
    /// - Jokaiselle mittarille tulostetaan `# TYPE`-rivi ja arvot.
    /// - Histogrammit tulostavat `_bucket{le="…"}`-rivit (kumulatiivinen),
    ///   `_sum`- ja `_count`-rivit Prometheus-konvention mukaan.
    ///
    /// Tuloste päättyy aina rivinvaihtoon (tai on tyhjä jos mittareita ei ole).
    #[must_use]
    pub fn prometheus_export(&self) -> String {
        let guard = match self.inner.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut out = String::new();
        for (name, metric) in guard.iter() {
            match metric {
                Metric::Counter(c) => {
                    let _ = writeln!(out, "# TYPE {name} counter");
                    let _ = writeln!(out, "{name} {}", c.get());
                }
                Metric::Gauge(g) => {
                    let _ = writeln!(out, "# TYPE {name} gauge");
                    let _ = writeln!(out, "{name} {}", g.get());
                }
                Metric::Histogram(h) => {
                    let _ = writeln!(out, "# TYPE {name} histogram");
                    for (bound, cumulative) in h.cumulative_buckets() {
                        let _ = writeln!(
                            out,
                            "{name}_bucket{{le=\"{}\"}} {cumulative}",
                            bound.label()
                        );
                    }
                    let _ = writeln!(out, "{name}_sum {}", format_float(h.sum()));
                    let _ = writeln!(out, "{name}_count {}", h.count());
                }
            }
        }
        out
    }
}

/// Esinimetty laskuri: luodut tehtävät.
pub const COUNTER_TASKS_CREATED: &str = "tasks_created";
/// Esinimetty laskuri: valmistuneet tehtävät.
pub const COUNTER_TASKS_COMPLETED: &str = "tasks_completed";
/// Esinimetty laskuri: tehtävien luovutukset.
pub const COUNTER_TASK_HANDOFFS: &str = "task_handoffs";
/// Esinimetty laskuri: ehdotetut sopimukset (contract-net).
pub const COUNTER_CONTRACT_PROPOSED: &str = "contract_proposed";
/// Esinimetty laskuri: täytetyt sopimukset.
pub const COUNTER_CONTRACT_FULFILLED: &str = "contract_fulfilled";
/// Esinimetty laskuri: rikotut sopimukset.
pub const COUNTER_CONTRACT_BREACHED: &str = "contract_breached";
/// Esinimetty laskuri: agenttivuorot.
pub const COUNTER_AGENT_TURNS: &str = "agent_turns";
/// Esinimetty laskuri: LLM-kutsut.
pub const COUNTER_LLM_CALLS: &str = "llm_calls";
/// Esinimetty laskuri: LLM-varamallikutsut (failover).
pub const COUNTER_LLM_FALLBACKS: &str = "llm_fallbacks";
/// Esinimetty laskuri: durable-uudelleenajot (replay).
pub const COUNTER_DURABLE_REPLAYS: &str = "durable_replays";
/// Esinimetty laskuri: valmistuneet workflow-askeleet.
pub const COUNTER_WORKFLOW_STEPS_COMPLETED: &str = "workflow_steps_completed";

/// Esinimetty gauge: online olevien agenttien määrä.
pub const GAUGE_AGENTS_ONLINE: &str = "agents_online";

/// Kaikki esinimetyt laskurit laivueen oletuksina.
const FLEET_COUNTERS: [&str; 11] = [
    COUNTER_TASKS_CREATED,
    COUNTER_TASKS_COMPLETED,
    COUNTER_TASK_HANDOFFS,
    COUNTER_CONTRACT_PROPOSED,
    COUNTER_CONTRACT_FULFILLED,
    COUNTER_CONTRACT_BREACHED,
    COUNTER_AGENT_TURNS,
    COUNTER_LLM_CALLS,
    COUNTER_LLM_FALLBACKS,
    COUNTER_DURABLE_REPLAYS,
    COUNTER_WORKFLOW_STEPS_COMPLETED,
];

/// Muotoilee `f64`:n vakaaksi (deterministiseksi) Prometheus-merkkijonoksi.
///
/// Kokonaisluvut tulostuvat ilman desimaalipistettä (`1` eikä `1.0`), muut
/// arvot lyhimmällä tarkalla esityksellä (Rustin oletus-`Display`).
fn format_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        // Kokonaisluku: tulosta ilman desimaaleja.
        #[allow(clippy::cast_possible_truncation)]
        let as_i = v as i64;
        as_i.to_string()
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_inc_and_inc_by_are_atomic() {
        let c = Counter::new();
        assert_eq!(c.get(), 0);
        c.inc();
        c.inc();
        c.inc_by(5);
        assert_eq!(c.get(), 7);
    }

    #[test]
    fn counter_clone_shares_value() {
        let c = Counter::new();
        let clone = c.clone();
        c.inc_by(3);
        assert_eq!(clone.get(), 3);
        clone.inc();
        assert_eq!(c.get(), 4);
    }

    #[test]
    fn gauge_set_add_sub() {
        let g = Gauge::new();
        g.set(10);
        assert_eq!(g.get(), 10);
        g.add(5);
        assert_eq!(g.get(), 15);
        g.sub(20);
        assert_eq!(g.get(), -5);
    }

    #[test]
    fn gauge_clone_shares_value() {
        let g = Gauge::new();
        let clone = g.clone();
        g.set(42);
        assert_eq!(clone.get(), 42);
    }

    #[test]
    fn histogram_observe_buckets_sum_count() {
        let h = Histogram::with_buckets(&[1.0, 2.0, 5.0]);
        h.observe(0.5); // <=1, <=2, <=5
        h.observe(1.5); // <=2, <=5
        h.observe(3.0); // <=5
        h.observe(9.0); // none of the finite buckets, but counted in +Inf

        assert_eq!(h.count(), 4);
        // 0.5 + 1.5 + 3.0 + 9.0 = 14.0
        assert!((h.sum() - 14.0).abs() < 1e-9);

        let buckets = h.cumulative_buckets();
        // le=1 -> 1, le=2 -> 2, le=5 -> 3, +Inf -> 4
        assert_eq!(buckets[0], (BucketBound::Le(1.0), 1));
        assert_eq!(buckets[1], (BucketBound::Le(2.0), 2));
        assert_eq!(buckets[2], (BucketBound::Le(5.0), 3));
        assert_eq!(buckets[3], (BucketBound::PosInf, 4));
    }

    #[test]
    fn histogram_ignores_negative_and_nonfinite() {
        let h = Histogram::with_buckets(&[1.0]);
        h.observe(-1.0);
        h.observe(f64::NAN);
        h.observe(f64::INFINITY);
        assert_eq!(h.count(), 0);
        assert!((h.sum() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn histogram_bounds_are_cleaned_and_sorted() {
        // Sekava syöte: lajittelematon, duplikaatti, negatiivinen, NaN.
        let h = Histogram::with_buckets(&[5.0, 1.0, 1.0, -3.0, f64::NAN, 2.0]);
        let bounds: Vec<_> = h
            .cumulative_buckets()
            .into_iter()
            .filter_map(|(b, _)| match b {
                BucketBound::Le(v) => Some(v),
                BucketBound::PosInf => None,
            })
            .collect();
        assert_eq!(bounds, vec![1.0, 2.0, 5.0]);
    }

    #[test]
    fn registry_counter_is_idempotent() {
        let reg = MetricsRegistry::new();
        let a = reg.counter("hits");
        a.inc_by(2);
        let b = reg.counter("hits");
        // Sama kahva → näkee saman arvon.
        assert_eq!(b.get(), 2);
        b.inc();
        assert_eq!(a.get(), 3);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn registry_gauge_and_histogram_idempotent() {
        let reg = MetricsRegistry::new();
        let g1 = reg.gauge("temp");
        g1.set(7);
        assert_eq!(reg.gauge("temp").get(), 7);

        let h1 = reg.histogram("latency");
        h1.observe(0.5);
        assert_eq!(reg.histogram("latency").count(), 1);
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn registry_clone_shares_state() {
        let reg = MetricsRegistry::new();
        let clone = reg.clone();
        reg.counter("x").inc();
        assert_eq!(clone.counter("x").get(), 1);
    }

    #[test]
    fn prometheus_export_is_deterministic_and_sorted() {
        let reg = MetricsRegistry::new();
        // Lisää väärässä aakkosjärjestyksessä.
        reg.counter("zebra").inc_by(2);
        reg.gauge("alpha").set(-3);

        let out = reg.prometheus_export();
        // alpha tulee ennen zebraa (nimijärjestys).
        let expected = "\
# TYPE alpha gauge
alpha -3
# TYPE zebra counter
zebra 2
";
        assert_eq!(out, expected);
    }

    #[test]
    fn prometheus_export_histogram_golden() {
        let reg = MetricsRegistry::new();
        let h = reg.histogram_with_buckets("req_seconds", &[1.0, 2.0]);
        h.observe(0.5); // <=1, <=2
        h.observe(1.5); // <=2
        h.observe(3.0); // +Inf only

        let out = reg.prometheus_export();
        let expected = "\
# TYPE req_seconds histogram
req_seconds_bucket{le=\"1\"} 1
req_seconds_bucket{le=\"2\"} 2
req_seconds_bucket{le=\"+Inf\"} 3
req_seconds_sum 5
req_seconds_count 3
";
        assert_eq!(out, expected);
    }

    #[test]
    fn prometheus_export_empty_registry_is_empty_string() {
        let reg = MetricsRegistry::new();
        assert!(reg.prometheus_export().is_empty());
        assert!(reg.is_empty());
    }

    #[test]
    fn fleet_defaults_prenames_all_series() {
        let reg = MetricsRegistry::with_fleet_defaults();
        // 11 laskuria + 1 gauge.
        assert_eq!(reg.len(), 12);
        let out = reg.prometheus_export();
        assert!(out.contains("# TYPE tasks_created counter"));
        assert!(out.contains("tasks_created 0"));
        assert!(out.contains("# TYPE agents_online gauge"));
        assert!(out.contains("agents_online 0"));
    }

    #[test]
    fn format_float_renders_integers_without_decimals() {
        assert_eq!(format_float(1.0), "1");
        assert_eq!(format_float(0.0), "0");
        assert_eq!(format_float(2.5), "2.5");
        assert_eq!(format_float(-3.0), "-3");
    }

    #[test]
    fn wrong_type_reuse_returns_detached_handle() {
        let reg = MetricsRegistry::new();
        let _c = reg.counter("dup");
        // Pyydä sama nimi gaugena → ei kaadu, palauttaa irrallisen gaugen.
        let g = reg.gauge("dup");
        g.set(99);
        // Rekisterissä on edelleen vain laskuri; export näyttää sen tyypin.
        let out = reg.prometheus_export();
        assert!(out.contains("# TYPE dup counter"));
        assert_eq!(reg.len(), 1);
    }

    // --- Rinnakkaisuustodisteet (kilpailutestit, ei ajoituksen varassa) ---

    /// Säikeiden lukumäärä rinnakkaisuustesteissä.
    const CONCURRENCY_THREADS: usize = 16;
    /// Inkrementtien lukumäärä per säie.
    const CONCURRENCY_ITERS: u64 = 10_000;

    /// `Counter::inc` ei menetä päivityksiä kun monta säiettä kilpailee samasta
    /// jaetusta kahvasta. Kaikki säikeet vapautetaan yhtä aikaa
    /// [`std::sync::Barrier`]:lla, jotta kilpailu tapahtuu varmasti. Lopputulos
    /// on tarkka deterministinen summa (`N * M`), joten testi ei voi olla
    /// epävakaa (flaky) — joko atomi pitää tai ei.
    #[test]
    fn counter_inc_no_lost_updates_under_contention() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let counter = Counter::new();
        let barrier = Arc::new(Barrier::new(CONCURRENCY_THREADS));
        let mut handles = Vec::with_capacity(CONCURRENCY_THREADS);

        for _ in 0..CONCURRENCY_THREADS {
            let c = counter.clone();
            let b = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                b.wait();
                for _ in 0..CONCURRENCY_ITERS {
                    c.inc();
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread panicked");
        }

        let expected = CONCURRENCY_THREADS as u64 * CONCURRENCY_ITERS;
        assert_eq!(counter.get(), expected);
    }

    /// Sama kilpailutodiste mutta rekisterin kautta haetulle laskurille:
    /// `MetricsRegistry::counter` palauttaa get-or-create-kahvan ja kaikki
    /// säikeet inkrementoivat *samaa* atomia. Tarkka summa todistaa ettei
    /// rekisterin haku eikä atomi-inkrementti menetä päivityksiä.
    #[test]
    fn registry_counter_no_lost_updates_under_contention() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let reg = MetricsRegistry::new();
        let barrier = Arc::new(Barrier::new(CONCURRENCY_THREADS));
        let mut handles = Vec::with_capacity(CONCURRENCY_THREADS);

        for _ in 0..CONCURRENCY_THREADS {
            let reg = reg.clone();
            let b = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let c = reg.counter("shared_hits");
                b.wait();
                for _ in 0..CONCURRENCY_ITERS {
                    c.inc();
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread panicked");
        }

        let expected = CONCURRENCY_THREADS as u64 * CONCURRENCY_ITERS;
        assert_eq!(reg.counter("shared_hits").get(), expected);
        // Yksi nimi → yksi rekisteröity mittari, ei duplikaatteja.
        assert_eq!(reg.len(), 1);
    }

    /// `Histogram::observe` säilyttää tarkan kokonaismäärän ja per-ämpäri-
    /// laskurit kilpailun alla. Jokainen havainto (`1.0`) osuu kaikkiin
    /// äärellisiin ämpäreihin (`>= 1.0`) ja `+Inf`:iin. Summa ja
    /// kumulatiiviset ämpärit ovat tarkkoja → ei epävakautta.
    #[test]
    fn histogram_observe_no_lost_updates_under_contention() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let hist = Histogram::with_buckets(&[1.0, 2.0, 5.0]);
        let barrier = Arc::new(Barrier::new(CONCURRENCY_THREADS));
        let mut handles = Vec::with_capacity(CONCURRENCY_THREADS);

        for _ in 0..CONCURRENCY_THREADS {
            let h = hist.clone();
            let b = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                b.wait();
                for _ in 0..CONCURRENCY_ITERS {
                    h.observe(1.0);
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread panicked");
        }

        let total = CONCURRENCY_THREADS as u64 * CONCURRENCY_ITERS;
        assert_eq!(hist.count(), total);
        // total * 1.0 = total. Vältetään leveä `u64 as f64` -kasti käyttämällä
        // tarkkaa `u32`→`f64`-muunnosta (160 000 mahtuu `u32`:een ja on tarkka
        // `f64`:nä); jokainen havainto on `1.0` joten summa == total.
        let total_u32 = u32::try_from(total).expect("total fits in u32");
        let expected_sum = f64::from(total_u32);
        assert!((hist.sum() - expected_sum).abs() < 1e-6);
        // 1.0 <= jokainen raja {1,2,5} → jokainen kumulatiivinen ämpäri == total.
        let buckets = hist.cumulative_buckets();
        assert_eq!(buckets[0], (BucketBound::Le(1.0), total));
        assert_eq!(buckets[1], (BucketBound::Le(2.0), total));
        assert_eq!(buckets[2], (BucketBound::Le(5.0), total));
        assert_eq!(buckets[3], (BucketBound::PosInf, total));
    }

    // --- Reunatapaukset ---

    /// `Histogram::observe(0.0)` lasketaan kelvolliseksi havainnoksi: se osuu
    /// pienimpään ämpäriin (kaikki rajat ovat `> 0.0`), kasvattaa
    /// kokonaismäärää eikä muuta summaa.
    #[test]
    fn histogram_observe_zero_lands_in_lowest_bucket_and_counts() {
        let h = Histogram::with_buckets(&[0.5, 1.0, 2.0]);
        h.observe(0.0);

        assert_eq!(h.count(), 1);
        assert!((h.sum() - 0.0).abs() < 1e-9);

        let buckets = h.cumulative_buckets();
        // 0.0 <= 0.5 → osuu pienimpään (ja kumulatiivisesti kaikkiin) ämpäriin.
        assert_eq!(buckets[0], (BucketBound::Le(0.5), 1));
        assert_eq!(buckets[1], (BucketBound::Le(1.0), 1));
        assert_eq!(buckets[2], (BucketBound::Le(2.0), 1));
        assert_eq!(buckets[3], (BucketBound::PosInf, 1));
    }
}
