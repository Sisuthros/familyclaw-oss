//! A lightweight metrics registry ([`MetricsRegistry`]) and its metric types
//! ([`Counter`], [`Gauge`], [`Histogram`]).
//!
//! This module implements a **hand-written Prometheus text export** without
//! the heavy `metrics`/`opentelemetry` stacks (deliberately unlinked words) —
//! `FamilyClaw` values a small
//! (2-8 MB) binary. Metrics are atomic (`AtomicU64`/`AtomicI64`) and the
//! registry shares handles via `Arc`, so multiple threads can update the
//! same metric without lock contention on the update path.
//!
//! ## Principles
//! - **Idempotent handles.** `counter(name)`/`gauge(name)`/`histogram(name)`
//!   return the *same* handle for the same name (get-or-create). The handle
//!   is `Arc`-shared, so a clone sees the same values.
//! - **Deterministic export.** [`MetricsRegistry::prometheus_export`]
//!   orders metrics by name, so the output is stable (easy to
//!   golden-string test).
//! - **No locks on the hot path.** Only the registry's name-to-handle lookup
//!   takes a lock; the increments themselves are lock-free atomics.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// A monotonically increasing counter.
///
/// Suited to cumulative event counts (e.g. tasks created,
/// LLM calls). The value is `u64` and updates are atomic.
#[derive(Debug, Clone, Default)]
pub struct Counter {
    value: Arc<AtomicU64>,
}

impl Counter {
    /// Creates a new counter with value `0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Increments the counter by one.
    pub fn inc(&self) {
        self.inc_by(1);
    }

    /// Increments the counter by the given amount.
    pub fn inc_by(&self, delta: u64) {
        self.value.fetch_add(delta, Ordering::Relaxed);
    }

    /// Returns the counter's current value.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// A metric that can freely go up and down (gauge).
///
/// Suited to instantaneous values that can rise and fall (e.g. the number
/// of online agents). The value is a signed `i64`.
#[derive(Debug, Clone, Default)]
pub struct Gauge {
    value: Arc<AtomicI64>,
}

impl Gauge {
    /// Creates a new gauge with value `0`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the gauge's value.
    pub fn set(&self, value: i64) {
        self.value.store(value, Ordering::Relaxed);
    }

    /// Adds the given (positive) amount to the gauge.
    pub fn add(&self, delta: i64) {
        self.value.fetch_add(delta, Ordering::Relaxed);
    }

    /// Subtracts the given amount from the gauge.
    pub fn sub(&self, delta: i64) {
        self.value.fetch_sub(delta, Ordering::Relaxed);
    }

    /// Returns the gauge's current value.
    #[must_use]
    pub fn get(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// The histogram's fixed upper-bound buckets (`le`, "less than or equal").
///
/// Values are in seconds (the Prometheus convention for durations), but the
/// histogram can be used for any non-negative quantity.
const DEFAULT_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// A histogram with fixed buckets.
///
/// Stores the cumulative `le`-bucket distribution, the sum of observations,
/// and the total count — all in Prometheus-compatible form. Bucket bounds
/// are locked in at creation time, so the bounds stay stable for the
/// metric's entire lifetime.
#[derive(Debug, Clone)]
pub struct Histogram {
    /// Bucket upper bounds in ascending order (shared, immutable).
    bounds: Arc<[f64]>,
    /// Per-bucket counters (non-cumulative; export cumulates them).
    counts: Arc<[AtomicU64]>,
    /// Sum of observations, scaled as milli-observations (see [`Histogram::sum`]).
    sum_milli: Arc<AtomicU64>,
    /// Total number of observations.
    count: Arc<AtomicU64>,
}

/// The fixed-point conversion scale for the sum (3 decimal places).
///
/// An `f64` sum cannot be stored atomically and safely without a lock, so
/// the sum is kept as an integer (`value * 1000`). This is sufficient for
/// Prometheus durations (millisecond resolution) and makes the sum
/// deterministic.
const SUM_SCALE: f64 = 1000.0;

/// `u64::MAX` as the nearest `f64` value (~= 1.8446744e19), used to clamp
/// the sum's upper bound without a lossy cast on the hot path.
const U64_MAX_AS_F64: f64 = 18_446_744_073_709_551_615.0;

impl Histogram {
    /// Creates a histogram with the default buckets (`DEFAULT_BUCKETS`).
    #[must_use]
    pub fn new() -> Self {
        Self::with_buckets(&DEFAULT_BUCKETS)
    }

    /// Creates a histogram with the given bucket bounds.
    ///
    /// Bounds are cleaned up: non-finite and non-positive values are
    /// removed, the rest are sorted ascending and deduplicated. If no
    /// bounds remain, a single `+Inf`-equivalent bucket is used (all
    /// observations go into the `_count` sum).
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

    /// Records a single observation.
    ///
    /// A non-negative `value` is added to the smallest bucket whose upper
    /// bound it is at or below, and increments the total count and sum.
    /// Negative and non-finite values are silently ignored (the Prometheus
    /// histogram model doesn't support them).
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
        // Fixed-point scaled sum (in milli-units). Rounded to the nearest integer.
        let scaled = (value * SUM_SCALE).round();
        // Clamp so we don't overflow on cast for huge values.
        // `U64_MAX_AS_F64` is `u64::MAX` as the nearest `f64` value (an exact
        // cast isn't possible, so we use the constant to avoid a lossy cast
        // on the hot path).
        let scaled = scaled.clamp(0.0, U64_MAX_AS_F64);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let milli = scaled as u64;
        self.sum_milli.fetch_add(milli, Ordering::Relaxed);
    }

    /// Total number of observations (`_count`).
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Sum of observations (`_sum`), converted back to `f64` from
    /// milli-observations.
    #[must_use]
    pub fn sum(&self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let raw = self.sum_milli.load(Ordering::Relaxed) as f64;
        raw / SUM_SCALE
    }

    /// Returns the cumulative `le` buckets as pairs of `(upper bound,
    /// cumulative count)`. The last pair is always `(+Inf, total count)`.
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

/// A histogram bucket's upper bound, for Prometheus export.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BucketBound {
    /// A finite upper bound (`le="<value>"`).
    Le(f64),
    /// Positive infinity (`le="+Inf"`).
    PosInf,
}

impl BucketBound {
    /// Formats the `le` value as a Prometheus label string.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            BucketBound::Le(v) => format_float(v),
            BucketBound::PosInf => "+Inf".to_string(),
        }
    }
}

/// The kind of metric held inside the registry.
#[derive(Debug, Clone)]
enum Metric {
    Counter(Counter),
    Gauge(Gauge),
    Histogram(Histogram),
}

/// A thread-safe metrics registry.
///
/// Holds named metrics ([`Counter`], [`Gauge`], [`Histogram`]) and exports
/// them in a deterministic Prometheus text format. The registry is `Clone`
/// and shares its state via `Arc` — all clones see the same metrics.
#[derive(Debug, Clone, Default)]
pub struct MetricsRegistry {
    inner: Arc<RwLock<BTreeMap<String, Metric>>>,
}

impl MetricsRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a registry pre-populated with the counters and gauges used
    /// for a multi-agent fleet (see the module documentation).
    ///
    /// This guarantees that the export includes these series from the
    /// start (with value `0`), so dashboards don't "disappear" before the
    /// first event occurs.
    #[must_use]
    pub fn with_fleet_defaults() -> Self {
        let reg = Self::new();
        for name in FLEET_COUNTERS {
            let _ = reg.counter(name);
        }
        let _ = reg.gauge(GAUGE_AGENTS_ONLINE);
        reg
    }

    /// Gets or creates a named counter (idempotent).
    ///
    /// The same name always returns the same handle. If the name is
    /// already registered under a different metric type, a *new,
    /// detached* counter is returned and is not registered (a safe
    /// fallback for the type-mismatch case).
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

    /// Gets or creates a named gauge (idempotent).
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

    /// Gets or creates a named histogram with the default buckets
    /// (idempotent).
    #[must_use]
    pub fn histogram(&self, name: &str) -> Histogram {
        self.histogram_with_buckets(name, &DEFAULT_BUCKETS)
    }

    /// Gets or creates a named histogram with the given buckets.
    ///
    /// If the histogram already exists (with any buckets), the existing
    /// handle is returned — `buckets` is only honored on first creation.
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

    /// The number of registered metrics.
    #[must_use]
    pub fn len(&self) -> usize {
        let guard = match self.inner.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Exports all metrics in a deterministic Prometheus text format.
    ///
    /// - Metrics are ordered by name (guaranteed by the `BTreeMap`).
    /// - Each metric emits a `# TYPE` line followed by its value(s).
    /// - Histograms emit `_bucket{le="…"}` lines (cumulative), followed by
    ///   `_sum` and `_count` lines per Prometheus convention.
    ///
    /// The output always ends with a newline (or is empty if there are no
    /// metrics).
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

/// Pre-named counter: tasks created.
pub const COUNTER_TASKS_CREATED: &str = "tasks_created";
/// Pre-named counter: tasks completed.
pub const COUNTER_TASKS_COMPLETED: &str = "tasks_completed";
/// Pre-named counter: task handoffs.
pub const COUNTER_TASK_HANDOFFS: &str = "task_handoffs";
/// Pre-named counter: proposed contracts (contract-net).
pub const COUNTER_CONTRACT_PROPOSED: &str = "contract_proposed";
/// Pre-named counter: fulfilled contracts.
pub const COUNTER_CONTRACT_FULFILLED: &str = "contract_fulfilled";
/// Pre-named counter: breached contracts.
pub const COUNTER_CONTRACT_BREACHED: &str = "contract_breached";
/// Pre-named counter: agent turns.
pub const COUNTER_AGENT_TURNS: &str = "agent_turns";
/// Pre-named counter: LLM calls.
pub const COUNTER_LLM_CALLS: &str = "llm_calls";
/// Pre-named counter: LLM fallback calls (failover).
pub const COUNTER_LLM_FALLBACKS: &str = "llm_fallbacks";
/// Pre-named counter: durable replays.
pub const COUNTER_DURABLE_REPLAYS: &str = "durable_replays";
/// Pre-named counter: completed workflow steps.
pub const COUNTER_WORKFLOW_STEPS_COMPLETED: &str = "workflow_steps_completed";

/// Pre-named counter: tool calls issued in an agent's tool loop.
pub const COUNTER_TOOL_CALLS: &str = "tool_calls";

/// Pre-named gauge: number of agents currently online.
pub const GAUGE_AGENTS_ONLINE: &str = "agents_online";

/// All pre-named counters used as fleet defaults.
const FLEET_COUNTERS: [&str; 12] = [
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
    COUNTER_TOOL_CALLS,
];

/// Formats an `f64` as a stable (deterministic) Prometheus string.
///
/// Integers are printed without a decimal point (`1` rather than `1.0`),
/// other values use the shortest exact representation (Rust's default
/// `Display`).
fn format_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        // Integer value: print without decimals.
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
        // Messy input: unsorted, duplicate, negative, NaN.
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
        // Same handle -> sees the same value.
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
        // Added out of alphabetical order.
        reg.counter("zebra").inc_by(2);
        reg.gauge("alpha").set(-3);

        let out = reg.prometheus_export();
        // alpha comes before zebra (name ordering).
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
        // 12 counters + 1 gauge.
        assert_eq!(reg.len(), 13);
        let out = reg.prometheus_export();
        assert!(out.contains("# TYPE tasks_created counter"));
        assert!(out.contains("tasks_created 0"));
        assert!(out.contains("# TYPE agents_online gauge"));
        assert!(out.contains("agents_online 0"));
        // Tool-call counter is pre-named -> shows as 0 before the first call.
        assert!(out.contains("# TYPE tool_calls counter"));
        assert!(out.contains("tool_calls 0"));
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
        // Request the same name as a gauge -> doesn't panic, returns a detached gauge.
        let g = reg.gauge("dup");
        g.set(99);
        // The registry still holds only the counter; export reflects that type.
        let out = reg.prometheus_export();
        assert!(out.contains("# TYPE dup counter"));
        assert_eq!(reg.len(), 1);
    }

    // --- Concurrency proofs (race tests, not timing-dependent) ---

    /// Number of threads used in the concurrency tests.
    const CONCURRENCY_THREADS: usize = 16;
    /// Number of increments per thread.
    const CONCURRENCY_ITERS: u64 = 10_000;

    /// `Counter::inc` does not lose updates when many threads race on the
    /// same shared handle. All threads are released simultaneously via a
    /// [`std::sync::Barrier`] to guarantee contention actually happens. The
    /// result is an exact, deterministic sum (`N * M`), so the test can't be
    /// flaky — either the atomic holds or it doesn't.
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

    /// The same concurrency proof, but for a counter obtained through the
    /// registry: `MetricsRegistry::counter` returns a get-or-create handle
    /// and every thread increments *the same* atomic. The exact sum proves
    /// that neither the registry lookup nor the atomic increment loses
    /// updates.
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
        // One name -> one registered metric, no duplicates.
        assert_eq!(reg.len(), 1);
    }

    /// `Histogram::observe` preserves an exact total count and per-bucket
    /// counts under contention. Every observation (`1.0`) falls into all
    /// finite buckets (`>= 1.0`) and `+Inf`. The sum and cumulative buckets
    /// are exact -> no flakiness.
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
        // total * 1.0 = total. We avoid a wide `u64 as f64` cast by using an
        // exact `u32` -> `f64` conversion (160,000 fits in a `u32` and is
        // exact as `f64`); every observation is `1.0` so the sum == total.
        let total_u32 = u32::try_from(total).expect("total fits in u32");
        let expected_sum = f64::from(total_u32);
        assert!((hist.sum() - expected_sum).abs() < 1e-6);
        // 1.0 <= every bound {1,2,5} -> every cumulative bucket == total.
        let buckets = hist.cumulative_buckets();
        assert_eq!(buckets[0], (BucketBound::Le(1.0), total));
        assert_eq!(buckets[1], (BucketBound::Le(2.0), total));
        assert_eq!(buckets[2], (BucketBound::Le(5.0), total));
        assert_eq!(buckets[3], (BucketBound::PosInf, total));
    }

    // --- Edge cases ---

    /// `Histogram::observe(0.0)` counts as a valid observation: it falls
    /// into the smallest bucket (all bounds are `> 0.0`), increments the
    /// total count, and doesn't change the sum.
    #[test]
    fn histogram_observe_zero_lands_in_lowest_bucket_and_counts() {
        let h = Histogram::with_buckets(&[0.5, 1.0, 2.0]);
        h.observe(0.0);

        assert_eq!(h.count(), 1);
        assert!((h.sum() - 0.0).abs() < 1e-9);

        let buckets = h.cumulative_buckets();
        // 0.0 <= 0.5 -> falls into the smallest (and cumulatively, all) bucket.
        assert_eq!(buckets[0], (BucketBound::Le(0.5), 1));
        assert_eq!(buckets[1], (BucketBound::Le(1.0), 1));
        assert_eq!(buckets[2], (BucketBound::Le(2.0), 1));
        assert_eq!(buckets[3], (BucketBound::PosInf, 1));
    }
}
