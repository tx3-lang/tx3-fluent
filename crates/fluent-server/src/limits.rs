//! Public-service limits on transaction tools: a daily [`Quota`] per user and
//! a server-wide [`Gate`].
//!
//! A transaction tool call runs as
//! `gate.within_cutoff(quota.consume(sub) → gate.admit(prepare))`:
//!
//! 1. the whole call must finish within `limits.global_cutoff_secs`, or it
//!    fails as `resolver_timeout` with that cutoff as `timeout_secs`;
//! 2. the caller's request is counted against `limits.per_user_daily_quota`
//!    for the current UTC day; the request past the limit fails as
//!    `quota_exhausted`, with `details.resets_at` the next UTC midnight, and
//!    counts nothing;
//! 3. at most `limits.max_concurrent_resolutions` preparations run at once;
//!    a request that waits [`ADMISSION_WAIT`] for a slot fails as
//!    `resolver_unavailable` ("server busy").
//!
//! The engine bounds each resolver call by `limits.resolver_timeout_secs`
//! itself. Skill and address tools are neither counted nor gated.
//!
//! The quota lives in the [`Store`], so it survives restarts and holds across
//! processes sharing the file. A server without a store, or a session without
//! an authenticated principal, is not metered.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluent_core::FluentError;
use fluent_core::config::LimitsConfig;
use tokio::sync::Semaphore;
use tracing::info;

use crate::metrics;
use crate::store::Store;

/// How long a request waits for a resolution slot before failing as busy.
pub const ADMISSION_WAIT: Duration = Duration::from_secs(5);

const SECONDS_PER_DAY: u64 = 86_400;

/// The time a [`Quota`] counts days by; injected so tests can change day.
pub trait Clock: Send + Sync + 'static {
    /// The current time.
    fn now(&self) -> SystemTime;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The daily quota of prepared-transaction requests per user, counted in
/// the store by fixed UTC day.
#[derive(Clone)]
pub struct Quota {
    store: Store,
    limit: u32,
    clock: Arc<dyn Clock>,
}

impl Quota {
    /// Allows each user `limit` requests per UTC day, counted in `store`.
    pub fn new(store: Store, limit: u32) -> Quota {
        Quota {
            store,
            limit,
            clock: Arc::new(SystemClock),
        }
    }

    /// Counts days by `clock` instead of the system clock.
    pub fn with_clock(self, clock: Arc<dyn Clock>) -> Quota {
        Quota { clock, ..self }
    }

    /// The daily limit.
    pub fn limit(&self) -> u32 {
        self.limit
    }

    /// Counts one request by `sub`; fails as
    /// [`FluentError::QuotaExhausted`] when they already made `limit` today.
    pub async fn consume(&self, sub: &str) -> Result<(), FluentError> {
        let day = unix_day(self.clock.now());
        let day_number = i64::try_from(day).map_err(FluentError::internal)?;
        match self
            .store
            .consume_quota(sub, day_number, self.limit)
            .await?
        {
            Some(_) => Ok(()),
            None => {
                metrics::quota_rejected();
                info!("daily quota exhausted");
                Err(FluentError::QuotaExhausted {
                    limit: self.limit,
                    resets_at: utc_midnight(day + 1),
                })
            }
        }
    }
}

/// The server-wide bounds on preparations: how many run at once, how long a
/// request waits to start one, and how long a whole call may take.
#[derive(Debug, Clone)]
pub struct Gate {
    slots: Arc<Semaphore>,
    wait: Duration,
    cutoff: Duration,
}

impl Gate {
    /// A gate of `max_concurrent` slots, admitting within `wait` and
    /// cutting calls off after `cutoff`.
    pub fn new(max_concurrent: usize, wait: Duration, cutoff: Duration) -> Gate {
        Gate {
            slots: Arc::new(Semaphore::new(max_concurrent)),
            wait,
            cutoff,
        }
    }

    /// The gate `[limits]` configures, admitting within [`ADMISSION_WAIT`].
    pub fn from_limits(limits: &LimitsConfig) -> Gate {
        Gate::new(
            usize::try_from(limits.max_concurrent_resolutions).unwrap_or(usize::MAX),
            ADMISSION_WAIT,
            Duration::from_secs(limits.global_cutoff_secs),
        )
    }

    /// Runs `call`, failing as [`FluentError::ResolverTimeout`] when it takes
    /// longer than the cutoff. The call is dropped, and with it any slot it
    /// holds.
    pub async fn within_cutoff<T>(
        &self,
        call: impl Future<Output = Result<T, FluentError>>,
    ) -> Result<T, FluentError> {
        match tokio::time::timeout(self.cutoff, call).await {
            Ok(result) => result,
            Err(_) => {
                info!(cutoff_secs = self.cutoff.as_secs_f64(), "tool call cut off");
                Err(FluentError::ResolverTimeout {
                    timeout_secs: self.cutoff.as_secs(),
                })
            }
        }
    }

    /// Runs `preparation` in a slot, waiting for one at most the admission
    /// wait; fails as [`FluentError::ServerBusy`] when none frees up.
    pub async fn admit<T>(
        &self,
        preparation: impl Future<Output = Result<T, FluentError>>,
    ) -> Result<T, FluentError> {
        let permit = match tokio::time::timeout(self.wait, self.slots.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(closed)) => return Err(FluentError::internal(closed)),
            Err(_) => {
                info!("no resolution slot freed up; server busy");
                return Err(FluentError::ServerBusy {
                    waited_secs: self.wait.as_secs(),
                });
            }
        };
        let _inflight = metrics::Inflight::start();
        let result = preparation.await;
        drop(permit);
        result
    }
}

impl Default for Gate {
    fn default() -> Gate {
        Gate::from_limits(&LimitsConfig::default())
    }
}

/// What a handler applies to transaction tools.
#[derive(Clone, Default)]
pub struct Limits {
    /// Concurrency, admission and cutoff, for every session.
    pub gate: Gate,
    /// The per-user quota; `None` leaves calls unmetered.
    pub quota: Option<Quota>,
}

impl Limits {
    /// Runs `preparation` for `sub` under every limit; see the [module
    /// documentation](self) for the order and errors.
    pub async fn apply<T>(
        &self,
        sub: Option<&str>,
        preparation: impl Future<Output = Result<T, FluentError>>,
    ) -> Result<T, FluentError> {
        self.gate
            .within_cutoff(async {
                if let (Some(quota), Some(sub)) = (&self.quota, sub) {
                    quota.consume(sub).await?;
                }
                self.gate.admit(preparation).await
            })
            .await
    }
}

/// Days since the Unix epoch at `time`, in UTC.
fn unix_day(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / SECONDS_PER_DAY)
}

/// The start of UTC day `day` (days since the epoch), as RFC 3339.
fn utc_midnight(day: u64) -> String {
    let (year, month, date) = civil_from_days(day);
    format!("{year:04}-{month:02}-{date:02}T00:00:00Z")
}

/// The proleptic Gregorian date of `days` since 1970-01-01, after Howard
/// Hinnant's `civil_from_days`.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let date = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, date)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fluent_core::ErrorCode;

    use super::*;

    /// A clock tests move by hand.
    struct Manual(Mutex<SystemTime>);

    impl Manual {
        fn at(secs: u64) -> Arc<Manual> {
            Arc::new(Manual(Mutex::new(UNIX_EPOCH + Duration::from_secs(secs))))
        }

        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }
    }

    impl Clock for Manual {
        fn now(&self) -> SystemTime {
            *self.0.lock().unwrap()
        }
    }

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("fluent.sqlite"))
            .await
            .unwrap();
        (dir, store)
    }

    /// 2026-09-29T23:59:00Z.
    const LATE: u64 = 1_790_726_340;

    #[test]
    fn midnights_render_as_rfc3339() {
        assert_eq!(utc_midnight(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            utc_midnight(unix_day(UNIX_EPOCH + Duration::from_secs(LATE))),
            "2026-09-29T00:00:00Z"
        );
        // 2024-02-29 is day 19782.
        assert_eq!(utc_midnight(19_782), "2024-02-29T00:00:00Z");
        assert_eq!(utc_midnight(19_783), "2024-03-01T00:00:00Z");
    }

    #[tokio::test]
    async fn the_request_past_the_limit_fails_until_the_day_changes() {
        let (_dir, store) = store().await;
        let clock = Manual::at(LATE);
        let quota = Quota::new(store, 3).with_clock(clock.clone());
        for _ in 0..3 {
            quota.consume("alice").await.unwrap();
        }
        let err = quota.consume("alice").await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::QuotaExhausted);
        assert_eq!(
            err.details(),
            Some(serde_json::json!({ "limit": 3, "resets_at": "2026-09-30T00:00:00Z" }))
        );
        // Another user has their own count.
        quota.consume("bob").await.unwrap();

        clock.advance(Duration::from_secs(60));
        quota.consume("alice").await.unwrap();
    }

    #[tokio::test]
    async fn a_full_gate_turns_requests_away_after_the_wait() {
        let gate = Gate::new(1, Duration::from_millis(50), Duration::from_secs(10));
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let holder = {
            let gate = gate.clone();
            tokio::spawn(async move {
                gate.admit(async {
                    let _ = released.await;
                    Ok(())
                })
                .await
            })
        };
        tokio::task::yield_now().await;
        while gate.slots.available_permits() > 0 {
            tokio::task::yield_now().await;
        }

        let err = gate.admit(async { Ok(()) }).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::ResolverUnavailable);
        assert!(matches!(err, FluentError::ServerBusy { .. }), "{err:?}");

        release.send(()).unwrap();
        holder.await.unwrap().unwrap();
        gate.admit(async { Ok(()) }).await.unwrap();
    }

    #[tokio::test]
    async fn the_gate_runs_at_most_its_slots_at_once() {
        let gate = Gate::new(2, Duration::from_secs(5), Duration::from_secs(10));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let calls: Vec<_> = (0..6)
            .map(|_| {
                let (gate, running, peak) = (gate.clone(), running.clone(), peak.clone());
                tokio::spawn(async move {
                    gate.admit(async {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
                })
            })
            .collect();
        for call in calls {
            call.await.unwrap().unwrap();
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn the_cutoff_fails_a_slow_call_as_a_resolver_timeout() {
        let gate = Gate::new(1, Duration::from_secs(5), Duration::from_millis(50));
        let err = gate
            .within_cutoff(async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::ResolverTimeout);
        // The cut-off call gave its slot back.
        assert_eq!(gate.slots.available_permits(), 1);
    }
}
