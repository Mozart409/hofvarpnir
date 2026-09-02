//! Runtime-mutable settings, stored as a single row.
#![deny(clippy::arithmetic_side_effects, clippy::string_slice)]

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::DbError;

/// The singleton `runtime_settings` row. `None` in a tunable field means
/// "not set at the database layer" — the resolver falls back to env, then
/// to the compiled-in default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSettingsRow {
    pub indexing_paused_until: Option<DateTime<Utc>>,
    pub downloads_paused_until: Option<DateTime<Utc>>,
    pub max_concurrent_downloads: Option<i32>,
    pub max_indexers_per_tick: Option<i32>,
    pub rate_limit_delay_secs: Option<i32>,
    pub check_interval_secs: Option<i32>,
    pub cleanup_interval_secs: Option<i32>,
    pub drain_timeout_secs: Option<i32>,
    pub updated_at: Option<DateTime<Utc>>,
    pub updated_by: Option<String>,
}

/// Read the singleton settings row.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn get_runtime_settings(pool: &PgPool) -> Result<RuntimeSettingsRow, DbError> {
    let row = sqlx::query!(
        r#"
        SELECT indexing_paused_until  AS "indexing_paused_until?: DateTime<Utc>",
               downloads_paused_until AS "downloads_paused_until?: DateTime<Utc>",
               max_concurrent_downloads, max_indexers_per_tick,
               rate_limit_delay_secs, check_interval_secs,
               cleanup_interval_secs, drain_timeout_secs,
               updated_at AS "updated_at: DateTime<Utc>",
               updated_by
        FROM runtime_settings WHERE id = true
        "#
    )
    .fetch_one(pool)
    .await?;

    Ok(RuntimeSettingsRow {
        indexing_paused_until: row.indexing_paused_until,
        downloads_paused_until: row.downloads_paused_until,
        max_concurrent_downloads: row.max_concurrent_downloads,
        max_indexers_per_tick: row.max_indexers_per_tick,
        rate_limit_delay_secs: row.rate_limit_delay_secs,
        check_interval_secs: row.check_interval_secs,
        cleanup_interval_secs: row.cleanup_interval_secs,
        drain_timeout_secs: row.drain_timeout_secs,
        updated_at: Some(row.updated_at),
        updated_by: row.updated_by,
    })
}

/// A partial update to the singleton `runtime_settings` row.
///
/// Every tunable field is `Option<Option<T>>`: the outer `None` means "leave
/// this column untouched"; `Some(None)` clears the column back to `NULL` so
/// the resolver falls through to the env/default layers; `Some(Some(v))`
/// sets it explicitly. `updated_by` records who made this particular patch
/// and is always written (it is not itself patchable to "leave alone",
/// since every application of a patch has an actor, even if that actor is
/// unknown/system and thus `None`).
#[derive(Debug, Clone, Default)]
pub struct RuntimeSettingsPatch {
    pub indexing_paused_until: Option<Option<DateTime<Utc>>>,
    pub downloads_paused_until: Option<Option<DateTime<Utc>>>,
    pub max_concurrent_downloads: Option<Option<i32>>,
    pub max_indexers_per_tick: Option<Option<i32>>,
    pub rate_limit_delay_secs: Option<Option<i32>>,
    pub check_interval_secs: Option<Option<i32>>,
    pub cleanup_interval_secs: Option<Option<i32>>,
    pub drain_timeout_secs: Option<Option<i32>>,
    pub updated_by: Option<String>,
}

/// Apply a partial update to the singleton settings row and return the row
/// as it stands afterward.
///
/// Only fields present (outer `Some`) in `patch` are touched; the SQL is
/// built with explicit `SET` clauses per touched column rather than
/// `COALESCE`, so a `Some(None)` genuinely writes `NULL` instead of leaving
/// the previous value in place.
///
/// # Errors
///
/// Returns an error if the database operation fails.
pub async fn patch_runtime_settings(
    pool: &PgPool,
    patch: &RuntimeSettingsPatch,
) -> Result<RuntimeSettingsRow, DbError> {
    let mut builder = sqlx::QueryBuilder::new("UPDATE runtime_settings SET updated_at = now()");

    if let Some(value) = patch.indexing_paused_until {
        builder.push(", indexing_paused_until = ").push_bind(value);
    }
    if let Some(value) = patch.downloads_paused_until {
        builder.push(", downloads_paused_until = ").push_bind(value);
    }
    if let Some(value) = patch.max_concurrent_downloads {
        builder
            .push(", max_concurrent_downloads = ")
            .push_bind(value);
    }
    if let Some(value) = patch.max_indexers_per_tick {
        builder.push(", max_indexers_per_tick = ").push_bind(value);
    }
    if let Some(value) = patch.rate_limit_delay_secs {
        builder.push(", rate_limit_delay_secs = ").push_bind(value);
    }
    if let Some(value) = patch.check_interval_secs {
        builder.push(", check_interval_secs = ").push_bind(value);
    }
    if let Some(value) = patch.cleanup_interval_secs {
        builder.push(", cleanup_interval_secs = ").push_bind(value);
    }
    if let Some(value) = patch.drain_timeout_secs {
        builder.push(", drain_timeout_secs = ").push_bind(value);
    }
    builder
        .push(", updated_by = ")
        .push_bind(patch.updated_by.clone());
    builder.push(" WHERE id = true");

    builder.build().execute(pool).await?;

    get_runtime_settings(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_pool, run_migrations};
    use crate::runtime_config::{indefinite_pause, resolve};

    // Integration tests require a running database.
    // Run with: DATABASE_URL=postgres://... cargo test -p hof-core --all-features -- --include-ignored

    /// This is the exact boundary the resolver's unit tests never cross:
    /// writing the indefinite-pause sentinel through `patch_runtime_settings`,
    /// reading it back through `get_runtime_settings`, and confirming the
    /// resolved value still reports "no deadline". A prior sentinel choice
    /// (`DateTime::<Utc>::MAX_UTC`) passed every in-memory unit test but
    /// silently failed exactly this round trip, because Postgres's
    /// microsecond-resolution `timestamptz` truncated away the nanosecond
    /// remainder on write.
    #[tokio::test]
    #[ignore = "requires a running database (run with --include-ignored)"]
    async fn indefinite_pause_round_trips_through_postgres() {
        let pool = create_pool().await.expect("Failed to create pool");
        run_migrations(&pool)
            .await
            .expect("Failed to run migrations");

        let sentinel = indefinite_pause();
        let patch = RuntimeSettingsPatch {
            downloads_paused_until: Some(Some(sentinel)),
            ..RuntimeSettingsPatch::default()
        };
        patch_runtime_settings(&pool, &patch)
            .await
            .expect("Failed to patch runtime settings");

        let row = get_runtime_settings(&pool)
            .await
            .expect("Failed to read runtime settings");
        assert_eq!(row.downloads_paused_until, Some(sentinel));

        let effective = resolve(&row, &crate::config::EnvOverrides::default());
        assert_eq!(effective.next_pause_deadline(Utc::now()), None);

        // Cleanup: leave the singleton row as the other `#[ignore]`d tests
        // in this binary expect to find it.
        let cleanup = RuntimeSettingsPatch {
            downloads_paused_until: Some(None),
            ..RuntimeSettingsPatch::default()
        };
        patch_runtime_settings(&pool, &cleanup)
            .await
            .expect("Failed to reset runtime settings");
    }
}
