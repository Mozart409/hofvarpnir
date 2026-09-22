//! Coverage for the data migration that clears latched `unordered` entry-order
//! verdicts (`20260922120000_reset_unordered_entry_order`).
//!
//! # Why this test drives the migrator by hand
//!
//! Every other database test here is a plain `#[sqlx::test]`, which hands the
//! test a database with *all* migrations already applied. That is exactly what
//! makes it useless for a data migration: by the time the test body runs, the
//! `UPDATE` has already happened against an empty `sources` table, and there is
//! no way to seed the rows it is supposed to act on. A test written that way
//! passes whether the migration does anything or not.
//!
//! So this one opts out with `migrations = false`, applies everything *before*
//! the migration under test, seeds the stuck state that was observed in
//! production, and only then applies that one migration. The assertions
//! therefore describe the migration's own effect rather than the schema's
//! end state.
//!
//! The seeded state is the bug this exists for: `detect_entry_order` used to
//! answer `Unordered` for any inconclusive two-point date comparison, and
//! `update_source_entry_order` stamps `entry_order_detected_at` for every value
//! other than `unknown`. A failed lookup was therefore indistinguishable from a
//! real detection, and `should_redetect_order` skipped re-detection for 30 days
//! on the strength of it.

// `clippy.toml` exempts `#[test]` functions and `#[cfg(test)]` modules, but
// this is an integration-test crate whose failures live in plain helpers.
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use chrono::NaiveDate;
use hof_core::db::{self, CreateProfile, CreateSource, CreateUser};
use hof_core::domain::profile::{OutputPreset, Quality};
use hof_core::domain::source::{EntryOrder, SourceType};
use sqlx::PgPool;
use sqlx::migrate::{Migrate, Migrator};
use ulid::Ulid;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The migration under test, as its filename spells it.
const TARGET_VERSION: i64 = 20_260_922_120_000;

/// Apply every migration older than `TARGET_VERSION`, leaving that one unrun.
async fn migrate_up_to_target(pool: &PgPool) {
    let mut conn = pool.acquire().await.expect("acquire a connection");
    let conn = &mut *conn;

    conn.ensure_migrations_table(&MIGRATOR.table_name)
        .await
        .expect("create the migrations table");

    // `Migrator` carries the `.down.sql` halves as their own entries; running
    // one here would undo the migration that precedes it.
    for migration in MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && m.version < TARGET_VERSION)
    {
        conn.apply(&MIGRATOR.table_name, migration)
            .await
            .unwrap_or_else(|e| panic!("apply migration {}: {e}", migration.version));
    }
}

/// Apply the migration under test, and nothing else.
async fn apply_target_migration(pool: &PgPool) {
    let mut conn = pool.acquire().await.expect("acquire a connection");
    let conn = &mut *conn;

    // Not `find(..).unwrap_or(return)`: if the file is renamed or dropped, this
    // test has to fail loudly rather than quietly assert nothing.
    let migration = MIGRATOR
        .iter()
        .find(|m| m.version == TARGET_VERSION && !m.migration_type.is_down_migration())
        .expect("the reset-unordered-entry-order migration should exist");

    conn.apply(&MIGRATOR.table_name, migration)
        .await
        .expect("apply the migration under test");
}

/// Create a source owned by a throwaway user and profile.
async fn seed_source(pool: &PgPool, name: &str) -> Ulid {
    let user = db::create_user(
        pool,
        CreateUser {
            email: &format!("{name}@example.com"),
            name,
            password_hash: Some("$argon2id$v=19$m=16,t=2,p=1$dGVzdHNhbHQ$test"),
        },
    )
    .await
    .expect("create user");

    let profile = db::create_profile(
        pool,
        CreateProfile {
            user_id: user.id,
            name,
            quality: Quality::Best,
            output_preset: OutputPreset::Auto,
            naming_template: "%(title)s",
            output_dir: "/tmp/hofvarpnir-migration-test",
            include_livestreams: false,
            include_shorts: false,
            storage_quota_bytes: 1_000_000_000,
            retention_days: None,
        },
    )
    .await
    .expect("create profile");

    let source = db::create_source(
        pool,
        CreateSource {
            profile_id: profile.id,
            url: &format!("https://youtube.com/@{name}"),
            source_type: SourceType::Channel,
            custom_name: Some(name),
            index_frequency_secs: 259_200,
            cutoff_date: NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date"),
            retention_days: None,
        },
    )
    .await
    .expect("create source");

    source.id
}

/// Read `entry_order` as text alongside its detection timestamp.
///
/// The column is a Postgres enum, which does not decode into `String`
/// directly — hence the cast. Reading it as text rather than through
/// `db::get_source` keeps the assertion about what is in the table, not about
/// how the domain layer maps it.
async fn read_entry_order(
    pool: &PgPool,
    id: Ulid,
) -> (String, Option<chrono::DateTime<chrono::Utc>>) {
    sqlx::query_as::<_, (String, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT entry_order::text, entry_order_detected_at FROM sources WHERE id = $1",
    )
    .bind(id.to_string())
    .fetch_one(pool)
    .await
    .expect("read the source's entry order")
}

/// The migration clears a latched `unordered` verdict and its timestamp, and
/// leaves every real verdict alone.
///
/// Both halves matter. Clearing `entry_order` without clearing
/// `entry_order_detected_at` would leave the source in a state it could never
/// escape, and clearing rows indiscriminately would throw away correct
/// `ascending`/`descending` verdicts and force a needless full re-scan of
/// every source on the instance.
#[sqlx::test(migrations = false)]
async fn migration_clears_unordered_verdicts_and_spares_real_ones(pool: PgPool) {
    migrate_up_to_target(&pool).await;

    let stuck = seed_source(&pool, "stuck").await;
    let sound = seed_source(&pool, "sound").await;
    let untouched = seed_source(&pool, "untouched").await;

    db::update_source_entry_order(&pool, stuck, EntryOrder::Unordered)
        .await
        .expect("latch a bogus verdict");
    db::update_source_entry_order(&pool, sound, EntryOrder::Descending)
        .await
        .expect("record a real verdict");

    // Guard the premise: if persisting a verdict stopped stamping the
    // timestamp, the production state this migration exists to clean up would
    // not be what is seeded here, and the assertions below would prove nothing.
    let (order, detected_at) = read_entry_order(&pool, stuck).await;
    assert_eq!(order, "unordered");
    assert!(
        detected_at.is_some(),
        "persisting a verdict must stamp the detection time"
    );

    apply_target_migration(&pool).await;

    let (order, detected_at) = read_entry_order(&pool, stuck).await;
    assert_eq!(
        order, "unknown",
        "the latched non-verdict should be cleared"
    );
    assert!(
        detected_at.is_none(),
        "the timestamp must go too, or re-detection stays suppressed for \
         REDETECTION_DAYS and the reset achieves nothing"
    );

    let (order, detected_at) = read_entry_order(&pool, sound).await;
    assert_eq!(
        order, "descending",
        "a real verdict must survive — re-detecting every source would mean a \
         full re-scan of each one"
    );
    assert!(
        detected_at.is_some(),
        "a surviving verdict keeps its detection time"
    );

    let (order, detected_at) = read_entry_order(&pool, untouched).await;
    assert_eq!(
        order, "unknown",
        "a source that was never detected stays where it was"
    );
    assert!(detected_at.is_none());
}
