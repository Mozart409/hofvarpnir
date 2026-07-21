#!/usr/bin/env bash
#
# Build the hofvarpnir OCI image locally (via Nix) and push it to the homelab
# Harbor registry.
#
# Like CI (.github/workflows/release.yml), this compiles the `hofvarpnir`
# binary with `cargo build --release` and then wraps it in the
# `.#containerFromBinary` OCI image — no in-Nix Rust compilation, so the build
# is fast. Only the x86_64 image is built/pushed (Harbor only needs amd64).
#
# Safety checks before anything is built or pushed:
#   * the current session must be logged in to the Harbor registry
#   * the git working tree must be clean
#   * HEAD must point at an annotated/lightweight git tag
#   * that tag must match the workspace version in Cargo.toml
#
# Push target: homelab-harbor.dropbear-butterfly.ts.net/oyabu/hofvarpnir[:TAG]
# Pull example:
#   podman pull homelab-harbor.dropbear-butterfly.ts.net/oyabu/hofvarpnir:<version>
#
# Usage: ./push_harbor.sh
set -euo pipefail

REGISTRY="homelab-harbor.dropbear-butterfly.ts.net/oyabu"
REPOSITORY="hofvarpnir"

# Always operate from the repo root regardless of where we're invoked from.
cd "$(git rev-parse --show-toplevel)"

die() {
    echo "error: $*" >&2
    exit 1
}

# --- must be logged in to the Harbor registry -----------------------------
REGISTRY_HOST="${REGISTRY%%/*}"
if ! podman login --get-login "$REGISTRY_HOST" &>/dev/null; then
    die "not logged in to $REGISTRY_HOST — run 'podman login $REGISTRY_HOST' first."
fi

# --- the working tree must be clean ---------------------------------------
if [[ -n "$(git status --porcelain)" ]]; then
    die "git working tree is not clean — commit or stash your changes first."
fi

# --- HEAD must be exactly tagged ------------------------------------------
if ! TAG="$(git describe --exact-match --tags HEAD 2>/dev/null)"; then
    die "HEAD is not tagged — tag the release commit before pushing."
fi

# --- the tag must match the workspace version -----------------------------
# The version lives under [workspace.package] in the root Cargo.toml.
VERSION="$(awk '
    /^\[/                    { section = $0 }
    section == "[workspace.package]" && /^[[:space:]]*version[[:space:]]*=/ {
        gsub(/.*=[[:space:]]*"?/, ""); gsub(/".*/, ""); print; exit
    }
' Cargo.toml)"

[[ -n "$VERSION" ]] || die "could not read version from [workspace.package] in Cargo.toml."

TAG_VERSION="${TAG#v}" # tolerate both "v0.1.0" and "0.1.0"
if [[ "$TAG_VERSION" != "$VERSION" ]]; then
    die "git tag ($TAG -> $TAG_VERSION) does not match Cargo version ($VERSION)."
fi

echo "==> releasing version $VERSION (tag $TAG) to $REGISTRY"

# --- compile the release binary -------------------------------------------
# Built inside the `.#ci` dev shell (SQLX_OFFLINE etc.) to match CI exactly.
echo "==> building release binary (cargo build --release --bin hofvarpnir)"
nix develop .#ci --command cargo build --release --bin hofvarpnir

HOFVARPNIR_BINARY="$(pwd)/target/release/hofvarpnir"
[[ -x "$HOFVARPNIR_BINARY" ]] || die "release binary not found at $HOFVARPNIR_BINARY."
export HOFVARPNIR_BINARY

# --- OCI image metadata ---------------------------------------------------
# These env vars are consumed by flake.nix (via builtins.getEnv), which is why
# the build below must run with `--impure`. They mirror the CI workflow so the
# resulting labels match released images.
OCI_IMAGE_VERSION="$VERSION"
OCI_IMAGE_REVISION="$(git rev-parse HEAD)"
OCI_IMAGE_CREATED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
export OCI_IMAGE_VERSION OCI_IMAGE_REVISION OCI_IMAGE_CREATED

# --- build the OCI image with Nix -----------------------------------------
# `.#containerFromBinary` wraps the pre-built binary (docker-archive /
# buildLayeredImage). --fallback builds a dependency from source if its
# binary-cache substitute fails.
echo "==> building .#containerFromBinary (x86_64)"
nix build --impure --fallback .#containerFromBinary --out-link result

# --- load the image into podman -------------------------------------------
echo "==> loading image into podman"
loaded="$(podman load -i result 2>&1 | sed -n 's/^Loaded image: //p' | tail -n1)"
[[ -n "$loaded" ]] || die "failed to determine loaded image name from 'podman load'."
echo "==> loaded ${loaded}"

# --- tag and push ---------------------------------------------------------
remote="${REGISTRY}/${REPOSITORY}:${VERSION}"
remote_latest="${REGISTRY}/${REPOSITORY}:latest"

echo "==> tagging ${loaded} -> ${remote}"
podman tag "$loaded" "$remote"
podman tag "$loaded" "$remote_latest"

echo "==> pushing ${remote}"
podman push "$remote"

echo "==> pushing ${remote_latest}"
podman push "$remote_latest"

echo "==> done — pushed ${REPOSITORY} at version $VERSION"
