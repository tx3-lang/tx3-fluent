//! The hosted store: users and the registrations each one selected, in the
//! SQLite file at `[store].sqlite_path`.
//!
//! A user's [`UserScope`] is the selections whose registration is in the live
//! catalog at the revision it was selected at. A selection whose registration
//! has since changed revision is [stale](SelectionStatus::UpdateRequired) and
//! hides its tools until it is selected again; one whose registration is gone
//! is [removed](SelectionStatus::Removed). A revoked user sees no tools and
//! every call fails as `unauthorized`.
//!
//! The migrations under `crates/fluent-server/migrations` are embedded and
//! applied by [`Store::open`]. Changes made through a [`Store`] are announced
//! to [`Store::subscribe`]rs by subject; changes another process makes to the
//! same file, such as `fluent admin revoke`, take effect on the next request
//! without an announcement.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use fluent_core::{Catalog, FluentError};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tokio::sync::broadcast;

use crate::http::auth::Principal;
use crate::mcp::{Scoping, ToolScope};

/// How long a write waits for another connection's lock before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many unannounced changes a slow subscriber may fall behind by.
const CHANGES_CAPACITY: usize = 64;

/// The persistent store of users and their selections. Cloning shares the
/// connection pool and the change announcements.
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    changes: broadcast::Sender<String>,
}

/// A user known to the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct User {
    /// The OIDC subject.
    pub sub: String,
    /// The email the user last authenticated with, when the token had one.
    pub email: Option<String>,
    /// When the user was first seen, in Unix seconds.
    pub created_at: i64,
    /// When an operator revoked the user, in Unix seconds.
    pub revoked_at: Option<i64>,
}

impl User {
    /// Whether an operator revoked the user.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

/// A registration a user selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Selection {
    /// The registration slug.
    pub slug: String,
    /// The registration's revision when it was selected.
    pub revision: String,
    /// When it was selected, in Unix seconds.
    pub selected_at: i64,
}

/// A selection against the live catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionStatus {
    /// The registration is live at the selected revision: its tools are in
    /// scope.
    Active,
    /// The registration changed revision since it was selected; the user must
    /// select it again to see its tools.
    UpdateRequired,
    /// The registration is no longer in the catalog.
    Removed,
}

impl Selection {
    /// Where this selection stands against `catalog`.
    pub fn status(&self, catalog: &Catalog) -> SelectionStatus {
        match catalog.get(&self.slug) {
            None => SelectionStatus::Removed,
            Some(registration) if registration.revision() == self.revision => {
                SelectionStatus::Active
            }
            Some(_) => SelectionStatus::UpdateRequired,
        }
    }
}

/// Turns a registration on or off for a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select<'a> {
    /// Select the registration at `revision`, its current one.
    On {
        /// The registration's revision in the live catalog.
        revision: &'a str,
    },
    /// Deselect the registration.
    Off,
}

impl Store {
    /// Opens the database at `path`, creating the file when it is missing, and
    /// applies pending migrations. The directory must exist.
    pub async fn open(path: &Path) -> anyhow::Result<Store> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(BUSY_TIMEOUT)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .with_context(|| format!("opening the store {}", path.display()))?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .with_context(|| format!("migrating the store {}", path.display()))?;
        let (changes, _) = broadcast::channel(CHANGES_CAPACITY);
        Ok(Store { pool, changes })
    }

    /// Announces the subject of every selection change and revocation made
    /// through this store.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.changes.subscribe()
    }

    fn announce(&self, sub: &str) {
        // Nobody listening is not a failure.
        let _ = self.changes.send(sub.to_string());
    }

    /// Records that `sub` authenticated, with `email` when the token had one.
    /// Keeps the first-seen time and any revocation.
    pub async fn upsert_user(&self, sub: &str, email: Option<&str>) -> Result<User, FluentError> {
        sqlx::query_as(
            "INSERT INTO users (sub, email, created_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT (sub) DO UPDATE SET email = COALESCE(excluded.email, users.email) \
             RETURNING sub, email, created_at, revoked_at",
        )
        .bind(sub)
        .bind(email)
        .bind(now())
        .fetch_one(&self.pool)
        .await
        .map_err(FluentError::internal)
    }

    /// The user `sub`, when the store knows them.
    pub async fn user(&self, sub: &str) -> Result<Option<User>, FluentError> {
        sqlx::query_as("SELECT sub, email, created_at, revoked_at FROM users WHERE sub = ?1")
            .bind(sub)
            .fetch_optional(&self.pool)
            .await
            .map_err(FluentError::internal)
    }

    /// The registrations `sub` selected, by slug.
    pub async fn list_selections(&self, sub: &str) -> Result<Vec<Selection>, FluentError> {
        sqlx::query_as(
            "SELECT slug, revision, selected_at FROM selections WHERE sub = ?1 ORDER BY slug",
        )
        .bind(sub)
        .fetch_all(&self.pool)
        .await
        .map_err(FluentError::internal)
    }

    /// Selects registration `slug` for `sub`, recording the user when they
    /// are new, or deselects it. Selecting again records the new revision.
    pub async fn set_selection(
        &self,
        sub: &str,
        slug: &str,
        select: Select<'_>,
    ) -> Result<(), FluentError> {
        let mut tx = self.pool.begin().await.map_err(FluentError::internal)?;
        match select {
            Select::On { revision } => {
                let at = now();
                sqlx::query(
                    "INSERT INTO users (sub, created_at) VALUES (?1, ?2) \
                     ON CONFLICT (sub) DO NOTHING",
                )
                .bind(sub)
                .bind(at)
                .execute(&mut *tx)
                .await
                .map_err(FluentError::internal)?;
                sqlx::query(
                    "INSERT INTO selections (sub, slug, revision, selected_at) \
                     VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT (sub, slug) DO UPDATE \
                     SET revision = excluded.revision, selected_at = excluded.selected_at",
                )
                .bind(sub)
                .bind(slug)
                .bind(revision)
                .bind(at)
                .execute(&mut *tx)
                .await
                .map_err(FluentError::internal)?;
            }
            Select::Off => {
                sqlx::query("DELETE FROM selections WHERE sub = ?1 AND slug = ?2")
                    .bind(sub)
                    .bind(slug)
                    .execute(&mut *tx)
                    .await
                    .map_err(FluentError::internal)?;
            }
        }
        tx.commit().await.map_err(FluentError::internal)?;
        self.announce(sub);
        Ok(())
    }

    /// Revokes `sub`: they keep their selections but see no tools, and every
    /// call fails as `unauthorized`. Revoking an unknown subject records them
    /// as revoked; revoking twice keeps the first time.
    pub async fn revoke(&self, sub: &str) -> Result<User, FluentError> {
        let at = now();
        let user = sqlx::query_as(
            "INSERT INTO users (sub, created_at, revoked_at) VALUES (?1, ?2, ?2) \
             ON CONFLICT (sub) DO UPDATE SET revoked_at = COALESCE(users.revoked_at, ?2) \
             RETURNING sub, email, created_at, revoked_at",
        )
        .bind(sub)
        .bind(at)
        .fetch_one(&self.pool)
        .await
        .map_err(FluentError::internal)?;
        self.announce(sub);
        Ok(user)
    }

    /// Counts one request by `sub` on UTC day `day` (days since the Unix
    /// epoch) unless they already made `limit` that day. Returns the day's
    /// count including this request, or `None` when the quota is exhausted
    /// and nothing was counted. Forgets `sub`'s earlier days.
    pub async fn consume_quota(
        &self,
        sub: &str,
        day: i64,
        limit: u32,
    ) -> Result<Option<u32>, FluentError> {
        let mut tx = self.pool.begin().await.map_err(FluentError::internal)?;
        sqlx::query("DELETE FROM quota_usage WHERE sub = ?1 AND day < ?2")
            .bind(sub)
            .bind(day)
            .execute(&mut *tx)
            .await
            .map_err(FluentError::internal)?;
        // The conditional update leaves an exhausted row alone and returns
        // nothing, so the check and the increment are one statement.
        let count: Option<(u32,)> = sqlx::query_as(
            "INSERT INTO quota_usage (sub, day, count) VALUES (?1, ?2, 1) \
             ON CONFLICT (sub, day) DO UPDATE SET count = quota_usage.count + 1 \
             WHERE quota_usage.count < ?3 \
             RETURNING count",
        )
        .bind(sub)
        .bind(day)
        .bind(limit)
        .fetch_optional(&mut *tx)
        .await
        .map_err(FluentError::internal)?;
        tx.commit().await.map_err(FluentError::internal)?;
        Ok(count.map(|(count,)| count))
    }

    /// The scope of `sub` against `catalog`.
    pub fn scope_for(&self, sub: &str, catalog: &Arc<Catalog>) -> UserScope {
        UserScope {
            store: self.clone(),
            sub: sub.to_string(),
            catalog: Arc::clone(catalog),
        }
    }
}

/// The registrations one user selected that are live at the selected
/// revision; read from the store on every request.
pub struct UserScope {
    store: Store,
    sub: String,
    catalog: Arc<Catalog>,
}

impl UserScope {
    async fn active_slugs(&self) -> Result<Vec<String>, FluentError> {
        if self
            .store
            .user(&self.sub)
            .await?
            .is_some_and(|u| u.is_revoked())
        {
            return Err(FluentError::Unauthorized);
        }
        let selections = self.store.list_selections(&self.sub).await?;
        Ok(selections
            .into_iter()
            .filter(|s| s.status(&self.catalog) == SelectionStatus::Active)
            .map(|s| s.slug)
            .collect())
    }
}

impl ToolScope for UserScope {
    fn visible_slugs(&self) -> BoxFuture<'_, Result<Vec<String>, FluentError>> {
        self.active_slugs().boxed()
    }
}

/// Per-user scopes: each session sees what the principal that initialized
/// it selected. A session without a principal sees nothing.
pub struct UserScopes {
    store: Store,
    catalog: Arc<Catalog>,
}

impl UserScopes {
    /// Scopes sessions to the selections in `store` against `catalog`.
    pub fn new(store: Store, catalog: Arc<Catalog>) -> UserScopes {
        UserScopes { store, catalog }
    }
}

impl Scoping for UserScopes {
    fn scope_for<'a>(
        &'a self,
        principal: Option<&'a Principal>,
    ) -> BoxFuture<'a, Result<Arc<dyn ToolScope>, FluentError>> {
        async move {
            let principal = principal.ok_or(FluentError::Unauthorized)?;
            self.store
                .upsert_user(&principal.sub, principal.email.as_deref())
                .await?;
            let scope: Arc<dyn ToolScope> =
                Arc::new(self.store.scope_for(&principal.sub, &self.catalog));
            Ok(scope)
        }
        .boxed()
    }

    fn changes(&self) -> Option<broadcast::Receiver<String>> {
        Some(self.store.subscribe())
    }
}

/// The current time in Unix seconds.
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("fluent.sqlite"))
            .await
            .expect("open store");
        (dir, store)
    }

    #[tokio::test]
    async fn selections_are_set_replaced_and_cleared() {
        let (_dir, store) = store().await;
        store
            .set_selection("alice", "b", Select::On { revision: "r1" })
            .await
            .unwrap();
        store
            .set_selection("alice", "a", Select::On { revision: "r1" })
            .await
            .unwrap();
        store
            .set_selection("alice", "a", Select::On { revision: "r2" })
            .await
            .unwrap();
        let listed: Vec<(String, String)> = store
            .list_selections("alice")
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.slug, s.revision))
            .collect();
        assert_eq!(
            listed,
            [("a".into(), "r2".into()), ("b".into(), "r1".into())]
        );

        store
            .set_selection("alice", "a", Select::Off)
            .await
            .unwrap();
        assert_eq!(store.list_selections("alice").await.unwrap().len(), 1);
        assert!(store.list_selections("bob").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_keeps_creation_revocation_and_a_known_email() {
        let (_dir, store) = store().await;
        let first = store.upsert_user("alice", Some("a@x")).await.unwrap();
        assert!(!first.is_revoked());
        let revoked = store.revoke("alice").await.unwrap();
        assert!(revoked.is_revoked());

        let again = store.upsert_user("alice", None).await.unwrap();
        assert_eq!(again.email.as_deref(), Some("a@x"));
        assert_eq!(again.created_at, first.created_at);
        assert_eq!(again.revoked_at, revoked.revoked_at);
    }

    #[tokio::test]
    async fn revoking_an_unknown_subject_records_it_revoked() {
        let (_dir, store) = store().await;
        assert!(store.user("mallory").await.unwrap().is_none());
        store.revoke("mallory").await.unwrap();
        assert!(store.user("mallory").await.unwrap().unwrap().is_revoked());
    }

    #[tokio::test]
    async fn quotas_count_per_subject_and_day_up_to_the_limit() {
        let (_dir, store) = store().await;
        assert_eq!(store.consume_quota("alice", 10, 2).await.unwrap(), Some(1));
        assert_eq!(store.consume_quota("alice", 10, 2).await.unwrap(), Some(2));
        assert_eq!(store.consume_quota("alice", 10, 2).await.unwrap(), None);
        assert_eq!(store.consume_quota("alice", 10, 2).await.unwrap(), None);
        assert_eq!(store.consume_quota("bob", 10, 2).await.unwrap(), Some(1));
        assert_eq!(store.consume_quota("alice", 11, 2).await.unwrap(), Some(1));

        let days: Vec<(i64,)> = sqlx::query_as("SELECT day FROM quota_usage WHERE sub = 'alice'")
            .fetch_all(&store.pool)
            .await
            .unwrap();
        assert_eq!(days, [(11,)]);
    }

    #[tokio::test]
    async fn changes_are_announced_by_subject() {
        let (_dir, store) = store().await;
        let mut changes = store.subscribe();
        store
            .set_selection("alice", "a", Select::On { revision: "r" })
            .await
            .unwrap();
        store.revoke("bob").await.unwrap();
        assert_eq!(changes.recv().await.unwrap(), "alice");
        assert_eq!(changes.recv().await.unwrap(), "bob");
    }
}
