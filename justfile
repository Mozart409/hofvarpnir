# https://just.systems

set unstable
set dotenv-load

# Cachix binary cache name
cachix_cache := "hofvarpnir"

default:
    @just --list

clear:
    clear

# Podman commands
up: clear
    podman-compose -f containers/compose.dev.yml up -d --build --remove-orphans
    sleep 1

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
    sqlx mig run --database-url ${DATABASE_URL}

[working-directory('crates/hof-core')]
mig-revert: clear up
    sqlx mig revert --database-url ${DATABASE_URL}

[working-directory('crates/hof-core')]
mig-info: clear up
    sqlx mig info --database-url ${DATABASE_URL}

[working-directory('crates/hof-core')]
db-reset: clear up
    sqlx database drop --database-url ${DATABASE_URL} -y
    sqlx database create --database-url ${DATABASE_URL}

[working-directory('crates/hof-core')]
db-setup: clear up
    sqlx database drop --database-url ${DATABASE_URL} -y
    sqlx database create --database-url ${DATABASE_URL}
    sqlx mig run --database-url ${DATABASE_URL}

# SQLx offline mode - run after schema changes
# Uses --workspace, so a single merged .sqlx/ is written to the repo root
# (covers query! macros in every crate, not just hof-core).
[working-directory('crates/hof-core')]
prepare: clear mig-run
    cargo sqlx prepare --workspace -- --all-targets --all-features

[working-directory('crates/hof-core')]
prepare-check: clear
    cargo sqlx prepare --workspace --check -- --all-targets --all-features

# Code quality
deny: clear fmt
    cargo deny check

fmt: clear
    cargo fmt --all

fix: clear
    cargo clippy --fix --allow-dirty --allow-staged --all-targets --all-features

lint: clear
    cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic

# Development
dev: clear up
    cargo watch -c -x 'run -p hof-web --bin hofvarpnir'

# Tailwind CSS
[working-directory('crates/hof-web/assets')]
css-watch:
    tailwindcss -i input.css -o app.css --watch --minify

[working-directory('crates/hof-web/assets')]
css-build:
    tailwindcss -i input.css -o app.css --minify

# Run all tests (--test-threads=4 avoids a #[sqlx::test] parallelism race on many-core machines)
test: clear up
    cargo test --all-features -- --include-ignored --test-threads=4

# E2E API tests
e2e: clear mig-run
    cargo test --package hof-api --test e2e --all-features -- --test-threads=4

# CI simulation (requires database)
ci: clear mig-run
    SQLX_OFFLINE=true cargo build --release
    cargo test --all-features -- --include-ignored --test-threads=4
    cargo clippy --all-targets --all-features -- -D warnings

# Check Nix cache availability
cache-check-x86: clear
    nix build .#devShells.x86_64-linux.ci --dry-run

cache-check-arm: clear
    nix build .#devShells.aarch64-linux.ci --dry-run

build-oci: clear
    nix flake update
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

trivy: clear build-oci
    trivy image --input result --scanners vuln

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
