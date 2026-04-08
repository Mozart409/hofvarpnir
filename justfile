# https://just.systems

set unstable
set dotenv-load

default:
    @just --list

clear:
    clear

# Podman commands
up: clear
    podman-compose -f containers/compose.dev.yml up -d --build --remove-orphans

down: clear
    podman-compose -f containers/compose.dev.yml down

logs service="":
    podman-compose -f containers/compose.dev.yml logs -f {{service}}

# Database commands
[working-directory: 'crates/hof-core']
mig-add name: clear
    sqlx mig add -r {{name}}

[working-directory: 'crates/hof-core']
mig-run: clear up
    sqlx mig run --database-url ${DATABASE_URL}

[working-directory: 'crates/hof-core']
mig-revert: clear up
    sqlx mig revert --database-url ${DATABASE_URL}

[working-directory: 'crates/hof-core']
mig-info: clear up
    sqlx mig info --database-url ${DATABASE_URL}

[working-directory: 'crates/hof-core']
db-reset: clear up
    sqlx database drop --database-url ${DATABASE_URL} -y
    sqlx database create --database-url ${DATABASE_URL}

[working-directory: 'crates/hof-core']
db-setup: clear up
    sqlx database drop --database-url ${DATABASE_URL} -y
    sqlx database create --database-url ${DATABASE_URL}
    sqlx mig run --database-url ${DATABASE_URL}

# SQLx offline mode - run after schema changes
[working-directory: 'crates/hof-core']
prepare: clear mig-run
    cargo sqlx prepare --workspace -- --all-targets --all-features

[working-directory: 'crates/hof-core']
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
dev: clear
    cargo watch -c -x 'run -p hof-web --bin hofvarpnir'

# Tailwind CSS
[working-directory: 'crates/hof-web/assets']
css-watch:
    tailwindcss -i input.css -o app.css --watch --minify

[working-directory: 'crates/hof-web/assets']
css-build:
    tailwindcss -i input.css -o app.css --minify

# Testing
test: clear
    cargo test --all-features -- --include-ignored

# CI simulation
ci: clear
    SQLX_OFFLINE=true cargo build --release
    SQLX_OFFLINE=true cargo test --all-features
    cargo clippy --all-targets --all-features -- -D warnings

build-oci: clear
    nix build .#container

trivy: clear build-oci
    trivy image --input result --scanners vuln

