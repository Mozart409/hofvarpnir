# SQLx troubleshooting

Two failure modes that look like application bugs but are infrastructure/tooling
issues. Search this page by the error text.

## Migration checksum mismatch: "was previously applied but has been modified"

**Symptom:**

```
migration <version> was previously applied but has been modified
```

Seen from the pre-commit `sqlx-prepare` hook (`just prepare`), on every commit
from the point of corruption onward.

**Cause:** Applying a migration (`just mig-run` / `just prepare`) records its
checksum in `_sqlx_migrations`. If the migration file is reformatted
afterwards — most commonly by `sqruff fix`, run either manually or via the
pre-commit `sqruff` hook — the checksum on disk no longer matches the one
recorded in the dev database, and every subsequent `sqlx migrate` invocation
refuses to proceed.

**Fix:**

```bash
just mig-revert
just mig-run
```

This re-applies the migration and records a fresh checksum. Do **not**
hand-patch the `checksum` column in `_sqlx_migrations` — it works around the
symptom without fixing the underlying mismatch and leaves a landmine for the
next person who diffs the migration file against the database.

**Avoid it entirely** by ordering the work correctly when adding a migration:

1. Write the SQL.
2. `sqruff fix` it.
3. `just prepare` (runs `mig-run`, then regenerates `.sqlx/`).
4. `just lint`.
5. `just test`.

Run `sqruff fix` *before* the migration is applied, never after. See
`AGENTS.md` § Build/Test/Lint for why `just prepare` must precede lint and
test (offline query cache).

## postgres-test index corruption: `XX002`

**Symptom:** a test using `#[sqlx::test]` fails inside sqlx's own
`testing/mod.rs`, not in application code, e.g.:

```
error returned from database: heap tid from index tuple (0,52) points past
end of heap page line pointer array at offset 2 of block 1 in index
"databases_pkey"
```

(Postgres error code `XX002`, "internal error" / data corruption.)

**Cause:** the `postgres-test` service (`containers/compose.dev.yml`,
localhost:5433) runs with durability disabled — see AGENTS.md's Database
section — which trades crash-safety for speed on an instance that's rebuilt
from a migration on demand. An unclean shutdown can leave an index corrupted.
The affected table is sqlx's own test-database registry,
`_sqlx_test.databases`, not anything application-owned.

**Fix:**

```sql
REINDEX TABLE _sqlx_test.databases;
```

Run against `localhost:5433`. Confirmed to resolve it in practice: a suite
that failed 2 tests with `XX002` passed 369/0 immediately after the reindex.

**Do not chase this as a test bug.** It's infrastructure state, not a
regression in the code under test. It also has nothing to do with the
checksum issue above: that one lives in the dev database (5432) and blocks
`sqlx migrate`; this one lives in the test instance (5433) and blocks
`#[sqlx::test]`'s own bookkeeping. Because `#[sqlx::test]` migrates a fresh
database per test, running migrations against 5433 does not fix — or cause —
either problem.
