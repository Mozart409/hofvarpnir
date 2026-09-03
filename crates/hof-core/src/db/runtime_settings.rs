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
    use chrono::SubsecRound;

    use super::*;
    use crate::config::EnvOverrides;
    use crate::db::{CreateUser, create_user};
    use crate::runtime_config::{DEFAULT_MAX_CONCURRENT, Provenance, indefinite_pause, resolve};

    // Each `#[sqlx::test]` fn gets a freshly migrated, isolated database
    // (see AGENTS.md), so these run under plain `just test` / CI — no
    // `--include-ignored`, no shared-row cleanup.

    /// A microsecond-precision timestamp `hours` from now (negative for the
    /// past), truncated to the resolution Postgres's `timestamptz` actually
    /// stores. Without the truncation, `Utc::now()`'s sub-microsecond
    /// remainder would make a post-round-trip equality check fail exactly
    /// the way the indefinite-pause sentinel once did (see the doc comment
    /// on `indefinite_pause_round_trips_through_postgres` below).
    fn micros_from_now(hours: i64) -> DateTime<Utc> {
        // `checked_add_signed` avoids the bare `+` this file's
        // `#![deny(clippy::arithmetic_side_effects)]` forbids, matching the
        // idiom used for production `DateTime` arithmetic elsewhere in this
        // crate (e.g. `startup.rs`, `runtime_config.rs`,
        // `download_supervisor.rs`). `hours` is caller-supplied in this
        // file's tests (small literals only), so this is not purely
        // ceremonial.
        Utc::now()
            .checked_add_signed(chrono::Duration::hours(hours))
            .expect("test fixture timestamp stays within chrono's representable range")
            .trunc_subsecs(6)
    }

    /// Every tunable set to its own distinct value, so a wrong `push` order
    /// in the builder would show up as the wrong column getting the wrong
    /// value rather than silently succeeding.
    fn all_fields_patch() -> RuntimeSettingsPatch {
        RuntimeSettingsPatch {
            indexing_paused_until: Some(Some(micros_from_now(2))),
            downloads_paused_until: Some(Some(micros_from_now(3))),
            max_concurrent_downloads: Some(Some(11)),
            max_indexers_per_tick: Some(Some(13)),
            rate_limit_delay_secs: Some(Some(17)),
            check_interval_secs: Some(Some(19)),
            cleanup_interval_secs: Some(Some(23)),
            drain_timeout_secs: Some(Some(29)),
            updated_by: None,
        }
    }

    // NOTE: a fresh row has every tunable already NULL (the migration only
    // inserts `(id)`), so asserting an untouched column "is still None"
    // proves nothing about whether the patch left it alone versus nulling
    // it. These tests seed every column to a distinct non-NULL value with
    // `all_fields_patch()` first, so "unchanged" means "still the seeded
    // value" and a builder bug that nulls or overwrites the wrong column
    // is actually observable.

    #[sqlx::test]
    async fn empty_patch_is_valid_sql_and_leaves_tunables_unchanged(pool: PgPool) {
        let seeded = patch_runtime_settings(&pool, &all_fields_patch())
            .await
            .expect("seed every tunable with a non-NULL value");

        let after = patch_runtime_settings(&pool, &RuntimeSettingsPatch::default())
            .await
            .expect("empty patch must still be valid SQL");

        assert_eq!(after.indexing_paused_until, seeded.indexing_paused_until);
        assert_eq!(after.downloads_paused_until, seeded.downloads_paused_until);
        assert_eq!(
            after.max_concurrent_downloads,
            seeded.max_concurrent_downloads
        );
        assert_eq!(after.max_indexers_per_tick, seeded.max_indexers_per_tick);
        assert_eq!(after.rate_limit_delay_secs, seeded.rate_limit_delay_secs);
        assert_eq!(after.check_interval_secs, seeded.check_interval_secs);
        assert_eq!(after.cleanup_interval_secs, seeded.cleanup_interval_secs);
        assert_eq!(after.drain_timeout_secs, seeded.drain_timeout_secs);

        // `updated_by` has no "leave alone" state: every applied patch has
        // an actor, even an empty one, so a `None` here still overwrites
        // whatever attribution was previously on the row.
        assert_eq!(after.updated_by, None);
    }

    #[sqlx::test]
    async fn single_field_patch_first_field_changes_only_that_field(pool: PgPool) {
        let seeded = patch_runtime_settings(&pool, &all_fields_patch())
            .await
            .expect("seed every tunable with a distinct value");

        let new_value = micros_from_now(5);
        let patch = RuntimeSettingsPatch {
            indexing_paused_until: Some(Some(new_value)),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &patch)
            .await
            .expect("patch first field");

        assert_eq!(row.indexing_paused_until, Some(new_value));
        assert_eq!(row.downloads_paused_until, seeded.downloads_paused_until);
        assert_eq!(
            row.max_concurrent_downloads,
            seeded.max_concurrent_downloads
        );
        assert_eq!(row.max_indexers_per_tick, seeded.max_indexers_per_tick);
        assert_eq!(row.rate_limit_delay_secs, seeded.rate_limit_delay_secs);
        assert_eq!(row.check_interval_secs, seeded.check_interval_secs);
        assert_eq!(row.cleanup_interval_secs, seeded.cleanup_interval_secs);
        assert_eq!(row.drain_timeout_secs, seeded.drain_timeout_secs);
    }

    #[sqlx::test]
    async fn single_field_patch_middle_field_changes_only_that_field(pool: PgPool) {
        let seeded = patch_runtime_settings(&pool, &all_fields_patch())
            .await
            .expect("seed every tunable with a distinct value");

        let patch = RuntimeSettingsPatch {
            rate_limit_delay_secs: Some(Some(42)),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &patch)
            .await
            .expect("patch middle field");

        assert_eq!(row.indexing_paused_until, seeded.indexing_paused_until);
        assert_eq!(row.downloads_paused_until, seeded.downloads_paused_until);
        assert_eq!(
            row.max_concurrent_downloads,
            seeded.max_concurrent_downloads
        );
        assert_eq!(row.max_indexers_per_tick, seeded.max_indexers_per_tick);
        assert_eq!(row.rate_limit_delay_secs, Some(42));
        assert_eq!(row.check_interval_secs, seeded.check_interval_secs);
        assert_eq!(row.cleanup_interval_secs, seeded.cleanup_interval_secs);
        assert_eq!(row.drain_timeout_secs, seeded.drain_timeout_secs);
    }

    #[sqlx::test]
    async fn single_field_patch_last_field_changes_only_that_field(pool: PgPool) {
        let seeded = patch_runtime_settings(&pool, &all_fields_patch())
            .await
            .expect("seed every tunable with a distinct value");

        let patch = RuntimeSettingsPatch {
            drain_timeout_secs: Some(Some(3600)),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &patch)
            .await
            .expect("patch last field");

        assert_eq!(row.indexing_paused_until, seeded.indexing_paused_until);
        assert_eq!(row.downloads_paused_until, seeded.downloads_paused_until);
        assert_eq!(
            row.max_concurrent_downloads,
            seeded.max_concurrent_downloads
        );
        assert_eq!(row.max_indexers_per_tick, seeded.max_indexers_per_tick);
        assert_eq!(row.rate_limit_delay_secs, seeded.rate_limit_delay_secs);
        assert_eq!(row.check_interval_secs, seeded.check_interval_secs);
        assert_eq!(row.cleanup_interval_secs, seeded.cleanup_interval_secs);
        assert_eq!(row.drain_timeout_secs, Some(3600));
    }

    #[sqlx::test]
    async fn patch_sets_every_tunable_in_one_call(pool: PgPool) {
        let patch = all_fields_patch();
        let row = patch_runtime_settings(&pool, &patch)
            .await
            .expect("patch every field");

        assert_eq!(
            row.indexing_paused_until,
            patch.indexing_paused_until.flatten()
        );
        assert_eq!(
            row.downloads_paused_until,
            patch.downloads_paused_until.flatten()
        );
        assert_eq!(row.max_concurrent_downloads, Some(11));
        assert_eq!(row.max_indexers_per_tick, Some(13));
        assert_eq!(row.rate_limit_delay_secs, Some(17));
        assert_eq!(row.check_interval_secs, Some(19));
        assert_eq!(row.cleanup_interval_secs, Some(23));
        assert_eq!(row.drain_timeout_secs, Some(29));
    }

    /// `Some(None)` must genuinely write `NULL` rather than leave the
    /// previous value in place — this is the assertion that protects the
    /// "reset to default" PATCH semantics. A `COALESCE`-based implementation
    /// would pass every other test in this file and fail only this one.
    #[sqlx::test]
    async fn some_none_writes_null_and_reengages_default_fallback(pool: PgPool) {
        let set = RuntimeSettingsPatch {
            max_concurrent_downloads: Some(Some(9)),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &set)
            .await
            .expect("set max_concurrent_downloads");
        assert_eq!(row.max_concurrent_downloads, Some(9));

        let reset = RuntimeSettingsPatch {
            max_concurrent_downloads: Some(None),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &reset)
            .await
            .expect("reset max_concurrent_downloads to NULL");
        assert_eq!(row.max_concurrent_downloads, None);

        let effective = resolve(&row, &EnvOverrides::default());
        assert_eq!(
            effective.max_concurrent_downloads.value,
            DEFAULT_MAX_CONCURRENT
        );
        assert_eq!(
            effective.max_concurrent_downloads.provenance,
            Provenance::Default
        );
    }

    /// The complement of the `Some(None)` case: an outer `None` must leave a
    /// previously-set value alone, which is what makes PATCH partial rather
    /// than destructive.
    #[sqlx::test]
    async fn outer_none_leaves_previously_set_value_alone(pool: PgPool) {
        let set = RuntimeSettingsPatch {
            max_indexers_per_tick: Some(Some(7)),
            ..RuntimeSettingsPatch::default()
        };
        patch_runtime_settings(&pool, &set)
            .await
            .expect("set max_indexers_per_tick");

        let other = RuntimeSettingsPatch {
            rate_limit_delay_secs: Some(Some(2)),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &other)
            .await
            .expect("patch an unrelated field");

        assert_eq!(row.max_indexers_per_tick, Some(7));
        assert_eq!(row.rate_limit_delay_secs, Some(2));
    }

    #[sqlx::test]
    async fn updated_at_advances_on_every_patch(pool: PgPool) {
        let before = get_runtime_settings(&pool)
            .await
            .expect("read initial row")
            .updated_at
            .expect("row always has updated_at");

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        let patch = RuntimeSettingsPatch {
            check_interval_secs: Some(Some(120)),
            ..RuntimeSettingsPatch::default()
        };
        let after = patch_runtime_settings(&pool, &patch)
            .await
            .expect("patch check_interval_secs")
            .updated_at
            .expect("row always has updated_at");

        assert!(
            after > before,
            "updated_at must advance: {before} -> {after}"
        );
    }

    /// `updated_by TEXT REFERENCES users (id)`: a fresh `#[sqlx::test]`
    /// database has no rows in `users`, so this proves the FK path the API
    /// actually exercises (a real user id) works, not just `None`.
    #[sqlx::test]
    async fn updated_by_accepts_a_real_user_and_is_always_overwritten(pool: PgPool) {
        let user = create_user(
            &pool,
            CreateUser {
                email: "runtime-settings-patch@example.com",
                name: "Runtime Settings Patcher",
                password_hash: None,
            },
        )
        .await
        .expect("create user");

        let attributed = RuntimeSettingsPatch {
            max_concurrent_downloads: Some(Some(4)),
            updated_by: Some(user.id.to_string()),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &attributed)
            .await
            .expect("patch attributed to a real user");
        assert_eq!(row.updated_by, Some(user.id.to_string()));

        // `updated_by` has no "leave alone" state (unlike the tunables): a
        // later patch that doesn't carry it wipes the prior attribution
        // even though this patch only touches a tunable. That is worth
        // flagging to the API layer, since a PATCH that sets one tunable
        // without re-asserting `updated_by` will silently blank
        // attribution rather than preserving the last attributor.
        let unattributed = RuntimeSettingsPatch {
            max_indexers_per_tick: Some(Some(6)),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &unattributed)
            .await
            .expect("patch without updated_by");
        assert_eq!(row.max_indexers_per_tick, Some(6));
        assert_eq!(row.updated_by, None);
    }

    /// This is the exact boundary the resolver's unit tests never cross:
    /// writing the indefinite-pause sentinel through `patch_runtime_settings`,
    /// reading it back through `get_runtime_settings`, and confirming the
    /// resolved value still reports "no deadline". A prior sentinel choice
    /// (`DateTime::<Utc>::MAX_UTC`) passed every in-memory unit test but
    /// silently failed exactly this round trip, because Postgres's
    /// microsecond-resolution `timestamptz` truncated away the nanosecond
    /// remainder on write.
    #[sqlx::test]
    async fn indefinite_pause_round_trips_through_postgres(pool: PgPool) {
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

        let effective = resolve(&row, &EnvOverrides::default());
        assert_eq!(effective.next_pause_deadline(Utc::now()), None);
    }

    #[sqlx::test]
    async fn pause_columns_round_trip_and_drive_resolve_booleans(pool: PgPool) {
        let future = micros_from_now(1);
        let patch = RuntimeSettingsPatch {
            indexing_paused_until: Some(Some(future)),
            downloads_paused_until: Some(Some(future)),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &patch)
            .await
            .expect("set both pause columns to a future deadline");

        let now = Utc::now();
        let effective = resolve(&row, &EnvOverrides::default());
        assert!(effective.indexing_paused(now));
        assert!(effective.downloads_paused(now));

        let past = micros_from_now(-1);
        let patch = RuntimeSettingsPatch {
            indexing_paused_until: Some(Some(past)),
            downloads_paused_until: Some(Some(past)),
            ..RuntimeSettingsPatch::default()
        };
        let row = patch_runtime_settings(&pool, &patch)
            .await
            .expect("set both pause columns to a past deadline");

        let now = Utc::now();
        let effective = resolve(&row, &EnvOverrides::default());
        assert!(!effective.indexing_paused(now));
        assert!(!effective.downloads_paused(now));
    }

    /// The CHECK constraints are a documented backstop behind the API's own
    /// validation, not the only guard against out-of-range values.
    #[sqlx::test]
    async fn check_constraints_reject_out_of_range_bounded_fields(pool: PgPool) {
        let cases: [(&str, RuntimeSettingsPatch); 6] = [
            (
                "max_concurrent_downloads = 0",
                RuntimeSettingsPatch {
                    max_concurrent_downloads: Some(Some(0)),
                    ..RuntimeSettingsPatch::default()
                },
            ),
            (
                "max_indexers_per_tick = 0",
                RuntimeSettingsPatch {
                    max_indexers_per_tick: Some(Some(0)),
                    ..RuntimeSettingsPatch::default()
                },
            ),
            (
                "rate_limit_delay_secs = -1",
                RuntimeSettingsPatch {
                    rate_limit_delay_secs: Some(Some(-1)),
                    ..RuntimeSettingsPatch::default()
                },
            ),
            (
                "check_interval_secs = 0",
                RuntimeSettingsPatch {
                    check_interval_secs: Some(Some(0)),
                    ..RuntimeSettingsPatch::default()
                },
            ),
            (
                "cleanup_interval_secs = 0",
                RuntimeSettingsPatch {
                    cleanup_interval_secs: Some(Some(0)),
                    ..RuntimeSettingsPatch::default()
                },
            ),
            (
                "drain_timeout_secs = 0",
                RuntimeSettingsPatch {
                    drain_timeout_secs: Some(Some(0)),
                    ..RuntimeSettingsPatch::default()
                },
            ),
        ];

        for (label, patch) in cases {
            let result = patch_runtime_settings(&pool, &patch).await;
            let code = match &result {
                Err(DbError::ConnectionError(sqlx_err)) => sqlx_err
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code),
                _ => None,
            };
            // Postgres SQLSTATE 23514 is `check_violation`; matching on it
            // (rather than just "some sqlx error") rules out the failure
            // being a malformed query string instead of the CHECK firing.
            assert_eq!(
                code.as_deref(),
                Some("23514"),
                "expected a CHECK (23514) violation for {label}, got {result:?}"
            );
        }
    }
}
