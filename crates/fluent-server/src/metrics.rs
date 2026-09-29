//! Service measurements, rendered in the Prometheus text format at
//! `GET /metrics`.
//!
//! | Series | Kind | Labels | Measures |
//! |---|---|---|---|
//! | `fluent_prepare_total` | counter | `registration`, `tx`, `outcome` | Transaction tool calls; `outcome` is `ok` or the error code. |
//! | `fluent_prepare_duration_seconds` | histogram | `registration`, `tx`, `outcome` | Their duration, limits included. |
//! | `fluent_quota_rejections_total` | counter | | Calls refused as `quota_exhausted`. |
//! | `fluent_inflight_resolutions` | gauge | | Preparations holding a slot. |
//! | `fluent_sessions_active` | gauge | | Initialized MCP sessions. |
//! | `process_*` | | | CPU seconds, resident and virtual memory, threads and file descriptors of the process. |
//!
//! Labels are catalog identifiers and error codes only, so their
//! cardinality is bounded by the catalog. Nothing is recorded until
//! [`install`] runs, which `fluent serve --http` does.

use std::sync::OnceLock;
use std::time::Duration;

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use metrics_process::Collector;
use tracing::warn;

/// Transaction tool calls, by registration, transaction and outcome.
pub const PREPARE_TOTAL: &str = "fluent_prepare_total";
/// Transaction tool call durations, in seconds.
pub const PREPARE_DURATION: &str = "fluent_prepare_duration_seconds";
/// Calls refused because the caller's daily quota is exhausted.
pub const QUOTA_REJECTIONS: &str = "fluent_quota_rejections_total";
/// Preparations currently holding a slot.
pub const INFLIGHT: &str = "fluent_inflight_resolutions";
/// Initialized MCP sessions.
pub const SESSIONS: &str = "fluent_sessions_active";

/// Histogram buckets for [`PREPARE_DURATION`], in seconds: from a cache-fast
/// answer to the default global cutoff.
const DURATION_BUCKETS: &[f64] = &[
    0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0, 45.0, 60.0,
];

/// The installed recorder's renderer and the process collector.
pub struct Metrics {
    handle: PrometheusHandle,
    process: Collector,
}

impl Metrics {
    /// Every series, in the Prometheus text exposition format, with the
    /// process measurements taken now.
    pub fn render(&self) -> String {
        self.process.collect();
        self.handle.run_upkeep();
        self.handle.render()
    }
}

/// Installs the process-wide recorder once and returns it; later calls
/// return the same one.
pub fn install() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        // Only an empty bucket list is refused; without buckets the
        // histogram renders as a summary.
        let builder = PrometheusBuilder::new()
            .set_buckets_for_metric(
                Matcher::Full(PREPARE_DURATION.to_string()),
                DURATION_BUCKETS,
            )
            .unwrap_or_else(|err| {
                warn!("prepare duration buckets rejected: {err}");
                PrometheusBuilder::new()
            });
        let recorder = builder.build_recorder();
        let handle = recorder.handle();
        if metrics::set_global_recorder(recorder).is_err() {
            warn!("another metrics recorder is installed; /metrics will be incomplete");
        }

        describe_counter!(PREPARE_TOTAL, "Transaction tool calls by outcome.");
        describe_histogram!(
            PREPARE_DURATION,
            metrics::Unit::Seconds,
            "Transaction tool call duration, limits included."
        );
        describe_counter!(
            QUOTA_REJECTIONS,
            "Transaction tool calls refused as quota_exhausted."
        );
        describe_gauge!(INFLIGHT, "Preparations holding a resolution slot.");
        describe_gauge!(SESSIONS, "Initialized MCP sessions.");
        // Series without labels exist from the start, so a scrape shows
        // zero rather than nothing.
        counter!(QUOTA_REJECTIONS).increment(0);
        gauge!(INFLIGHT).increment(0.0);
        gauge!(SESSIONS).increment(0.0);

        let process = Collector::default();
        process.describe();
        Metrics { handle, process }
    })
}

/// Records one finished transaction tool call.
pub fn prepared(registration: &str, tx: &str, outcome: &str, duration: Duration) {
    let labels = [
        ("registration", registration.to_string()),
        ("tx", tx.to_string()),
        ("outcome", outcome.to_string()),
    ];
    counter!(PREPARE_TOTAL, &labels).increment(1);
    histogram!(PREPARE_DURATION, &labels).record(duration.as_secs_f64());
}

/// Records one call refused because the caller's quota is exhausted.
pub fn quota_rejected() {
    counter!(QUOTA_REJECTIONS).increment(1);
}

/// Counts one preparation in [`INFLIGHT`] while it lives.
pub struct Inflight(());

impl Inflight {
    /// Starts counting.
    pub fn start() -> Inflight {
        gauge!(INFLIGHT).increment(1.0);
        Inflight(())
    }
}

impl Drop for Inflight {
    fn drop(&mut self) {
        gauge!(INFLIGHT).decrement(1.0);
    }
}

/// Counts one session in [`SESSIONS`] while it lives.
#[derive(Debug)]
pub struct Session(());

impl Session {
    /// Starts counting.
    pub fn start() -> Session {
        gauge!(SESSIONS).increment(1.0);
        Session(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        gauge!(SESSIONS).decrement(1.0);
    }
}
