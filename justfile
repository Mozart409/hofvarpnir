# https://just.systems

set unstable

cachix_cache := "hofvarpnir"

# Secrets and app config live encrypted in .sops.env (sops + gpg-agent; values
# ENC[...], names visible, safe to commit). They are decrypted into the ONE
# child process that needs them -- the app itself, via `dev` -- and never
# written to disk or exported into the shell. Edit with `sops .sops.env`.
# (`set dotenv-load` and a plaintext .env are gone on purpose.)
secrets := "sops exec-env .sops.env"

# Dev Postgres (postgres service in containers/compose.dev.yml). The credentials
# are compose's own, not a secret, so the DB tooling below uses this directly
# instead of decrypting .sops.env for every migration. Override with
# DATABASE_URL to point elsewhere.
database_url := env_var_or_default("DATABASE_URL", "postgresql://postgres:postgres@localhost:5432/hofvarpnir_dev")

# Lean, ephemeral Postgres used exclusively by `just test` (postgres-test service
# in containers/compose.dev.yml). Override with TEST_DATABASE_URL to point the
# suite elsewhere.
test_database_url := env_var_or_default("TEST_DATABASE_URL", "postgresql://postgres:postgres@localhost:5433/postgres")

default:
    @just --list

clear:
    clear || true

# Podman commands
# Short-circuits when the database is already accepting connections, so recipes
# that depend on this one (mig-run, test, dev, ...) don't pay for compose.
up: clear
    #!/usr/bin/env bash
    set -euo pipefail
    if pg_isready -d "{{ database_url }}" -t 2 -q \
        && pg_isready -d "{{ test_database_url }}" -t 2 -q; then
        echo "databases already available, skipping podman-compose"
        exit 0
    fi
    podman-compose -f containers/compose.dev.yml up -d --build --remove-orphans
    # Wait until postgres actually answers (compose returns before readiness)
    for _ in $(seq 1 30); do
        if pg_isready -d "{{ database_url }}" -t 1 -q \
            && pg_isready -d "{{ test_database_url }}" -t 1 -q; then
            exit 0
        fi
        sleep 1
    done
    echo "databases did not become ready within 30s" >&2
    exit 1

down: clear
    podman-compose -f containers/compose.dev.yml down

down-v: clear
    podman-compose -f containers/compose.dev.yml down -v

logs service="":
    podman-compose -f containers/compose.dev.yml logs -f {{ service }}

# Database commands
[working-directory('crates/hof-core')]
mig-add name: clear
    sqlx mig add -r {{ name }}

[working-directory('crates/hof-core')]
mig-run: clear up
    sqlx mig run --database-url {{ database_url }}

[working-directory('crates/hof-core')]
mig-revert: clear up
    sqlx mig revert --database-url {{ database_url }}

[working-directory('crates/hof-core')]
mig-info: clear up
    sqlx mig info --database-url {{ database_url }}

[working-directory('crates/hof-core')]
db-reset: clear up
    sqlx database drop --database-url {{ database_url }} -y
    sqlx database create --database-url {{ database_url }}

[working-directory('crates/hof-core')]
db-setup: clear up
    sqlx database drop --database-url {{ database_url }} -y
    sqlx database create --database-url {{ database_url }}
    sqlx mig run --database-url {{ database_url }}

# SQLx offline mode - run after schema changes
# Uses --workspace, so a single merged .sqlx/ is written to the repo root

# (covers query! macros in every crate, not just hof-core).
[working-directory('crates/hof-core')]
prepare: clear mig-run
    DATABASE_URL={{ database_url }} cargo sqlx prepare --workspace -- --all-targets --all-features

[working-directory('crates/hof-core')]
prepare-check: clear
    DATABASE_URL={{ database_url }} cargo sqlx prepare --workspace --check -- --all-targets --all-features

# Code quality
deny: clear fmt
    cargo deny check

fmt: clear
    cargo fmt --all

fix: clear
    SQLX_OFFLINE=true cargo clippy --fix --allow-dirty --allow-staged --all-targets --all-features

lint: clear
    SQLX_OFFLINE=true cargo clippy --workspace --all-targets --all-features -- -D warnings

# Development. The only recipe that needs the real config: the binary reads
# everything from its environment (dotenvy finds no .env any more).
dev: clear up
    {{ secrets }} 'cargo watch -c -x "run -p hof-web --bin hofvarpnir"'

# Tailwind CSS
[working-directory('crates/hof-web/assets')]
css-watch:
    tailwindcss -i input.css -o app.css --watch --minify

[working-directory('crates/hof-web/assets')]
css-build:
    tailwindcss -i input.css -o app.css --minify

# Run all tests against the lean postgres-test instance
# (--test-threads=4 avoids a #[sqlx::test] parallelism race on many-core machines)
#
# SQLX_OFFLINE=true is required: the postgres-test instance is intentionally
# lean and carries no schema, and while #[sqlx::test] migrates each per-test
# database at runtime, the query!/query_as! macros validate against the live
# DATABASE_URL at *compile* time. Without offline mode every query fails to
# compile with `relation "sources" does not exist`. Compilation uses the
# committed .sqlx cache; the tests still talk to postgres-test at runtime.
# Run `just prepare` after any schema change to keep that cache current.
test: clear up
    SQLX_OFFLINE=true DATABASE_URL={{ test_database_url }} cargo test --all-features -- --include-ignored --test-threads=4

# E2E API tests against the lean postgres-test instance.
# (#[sqlx::test] migrates each test database itself, so this only needs `up`)
e2e: clear up
    SQLX_OFFLINE=true DATABASE_URL={{ test_database_url }} cargo test --package hof-api --test e2e --all-features -- --test-threads=4

# Same as `e2e`, but skips `up` — works when an unrelated container in the
# compose stack (e.g. grafana) is failing to start. Requires postgres-test
# to already be running.
e2e-only: clear
    SQLX_OFFLINE=true DATABASE_URL={{ test_database_url }} cargo test --package hof-api --test e2e --all-features -- --test-threads=4

# CI simulation (requires database; tests run against the lean postgres-test instance)
ci: clear up
    SQLX_OFFLINE=true cargo build --release
    SQLX_OFFLINE=true DATABASE_URL={{ test_database_url }} cargo test --all-features -- --include-ignored --test-threads=4
    SQLX_OFFLINE=true cargo clippy --workspace --all-targets --all-features -- -D warnings

# Check Nix cache availability
cache-check-x86: clear
    nix build .#devShells.x86_64-linux.ci --dry-run

cache-check-arm: clear
    nix build .#devShells.aarch64-linux.ci --dry-run

build-oci: clear
    nix build .#container

# `cachix push` uploads the out-path's full closure, so this works even when
# the build is already realized locally (unlike `watch-exec`, which only
# catches paths built during the wrapped command).

# Build the x86 container and push it to Cachix.
cachix: clear
    nix build --no-link --print-out-paths .#container | cachix push {{ cachix_cache }}

# Push any flake attribute's closure to Cachix, e.g. .#devShells.aarch64-linux.ci
cachix-push attr: clear
    nix build --no-link --print-out-paths {{ attr }} | cachix push {{ cachix_cache }}

# aarch64 is realized locally via binfmt/qemu, so this needs
# `extra-platforms = aarch64-linux` in your Nix config (set by NixOS
# `boot.binfmt.emulatedSystems`; verify: nix config show extra-platforms).

# Warm Cachix for both arches (the CI devshells cache-check-{x86,arm} verify).
cache-warm: clear
    nix build --no-link --print-out-paths \
      .#devShells.x86_64-linux.ci \
      .#devShells.aarch64-linux.ci \
      | cachix push {{ cachix_cache }}

# Two-way sync with GitHub (origin) as the source of truth. Bots push only to
# forgejo, so their branches flow up to origin first; anything origin has is
# then force-mirrored onto forgejo, so a diverged branch or tag resolves to
# GitHub's version. Forgejo-only branches are never deleted.
sync-remotes: clear
    #!/usr/bin/env bash
    set -euo pipefail
    git fetch origin --prune --prune-tags --force
    git fetch forgejo --prune
    # Bot work flows up: a forgejo branch origin lacks, or that fast-forwards
    # origin's, is pushed to origin. Diverged branches are left alone here and
    # lose to origin in the mirror step below.
    for b in $(git for-each-ref --format='%(refname:lstrip=3)' refs/remotes/forgejo/ | grep -vx HEAD); do
        if ! git show-ref -q --verify "refs/remotes/origin/$b" \
           || git merge-base --is-ancestor "refs/remotes/origin/$b" "refs/remotes/forgejo/$b"; then
            git push origin "refs/remotes/forgejo/$b:refs/heads/$b"
        fi
    done
    git push origin --all
    git push origin --tags
    git fetch origin --prune
    # origin wins: force-mirror every branch and tag it has onto forgejo.
    git push forgejo --force 'refs/remotes/origin/*:refs/heads/*' '^refs/remotes/origin/HEAD'
    git push forgejo --force 'refs/tags/*:refs/tags/*'
    git fetch forgejo --prune

trivy: clear build-oci
    trivy image --input result --scanners vuln --ignorefile .trivyignore.yaml

# No version/tag safety checks — use ./push_harbor.sh for versioned releases.
# Compile the release binary, wrap it in the x86 OCI image via

# `.#containerFromBinary`, and push to Harbor as :dev (fast dev push).
push-oci: clear
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --bin hofvarpnir
    export HOFVARPNIR_BINARY="$(pwd)/target/release/hofvarpnir"
    nix build --impure --fallback .#containerFromBinary --out-link result
    loaded="$(podman load -i result 2>&1 | sed -n 's/^Loaded image: //p' | tail -n1)"
    podman tag "$loaded" homelab-harbor.dropbear-butterfly.ts.net/oyabu/hofvarpnir:dev
    podman push homelab-harbor.dropbear-butterfly.ts.net/oyabu/hofvarpnir:dev
