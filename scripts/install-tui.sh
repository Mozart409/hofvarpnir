#!/usr/bin/env bash
# Install the Hofvarpnir TUI from the latest GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/Mozart409/hofvarpnir/main/scripts/install-tui.sh | bash
#
# Environment overrides:
#   HOFVARPNIR_VERSION     Version to install, with or without the leading "v"
#                          (default: latest release, resolved via the GitHub API)
#   HOFVARPNIR_INSTALL_DIR Install directory (default: ~/.local/bin)
set -euo pipefail

REPO="Mozart409/hofvarpnir"
BINARY="hofvarpnir-tui"
INSTALL_DIR="${HOFVARPNIR_INSTALL_DIR:-${HOME}/.local/bin}"

info() { printf 'info: %s\n' "$*"; }
warn() { printf 'warn: %s\n' "$*" >&2; }
fatal() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fatal "required tool not found: $1"
}

need curl
need tar

# --- platform detection ----------------------------------------------------

case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  aarch64 | arm64) arch="aarch64" ;;
  *) fatal "unsupported architecture: $(uname -m)" ;;
esac

case "$(uname -s)" in
  Linux)
    if ldd --version 2>&1 | grep -qi musl || ls /lib/ld-musl-* >/dev/null 2>&1; then
      target="${arch}-unknown-linux-musl"
    else
      target="${arch}-unknown-linux-gnu"
    fi
    ;;
  Darwin)
    target="${arch}-apple-darwin"
    ;;
  *) fatal "unsupported OS: $(uname -s)" ;;
esac

# --- version resolution ------------------------------------------------------

version="${HOFVARPNIR_VERSION:-}"
if [ -z "${version}" ]; then
  info "resolving latest release..."
  version="$(
    curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
      | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p'
  )"
  [ -n "${version}" ] || fatal "could not determine latest release (GitHub API rate limit?); set HOFVARPNIR_VERSION"
fi
version="${version#v}"

archive="${BINARY}-${target}-v${version}.tar.gz"
base_url="https://github.com/${REPO}/releases/download/v${version}"

info "installing ${BINARY} v${version} for ${target}"

# --- download and verify -----------------------------------------------------

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

curl -fsSL "${base_url}/${archive}" -o "${tmp_dir}/${archive}" \
  || fatal "download failed: ${base_url}/${archive}"

if curl -fsSL "${base_url}/${archive}.sha256" -o "${tmp_dir}/${archive}.sha256" 2>/dev/null; then
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "${tmp_dir}/${archive}" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "${tmp_dir}/${archive}" | cut -d' ' -f1)"
  else
    fatal "no sha256sum or shasum available to verify the download"
  fi
  expected="$(tr -d '[:space:]' <"${tmp_dir}/${archive}.sha256")"
  [ "${actual}" = "${expected}" ] || fatal "checksum mismatch for ${archive} (expected ${expected}, got ${actual})"
  info "checksum verified"
else
  warn "no checksum file found for this release; skipping verification"
fi

# --- install -----------------------------------------------------------------

tar -xzf "${tmp_dir}/${archive}" -C "${tmp_dir}"
[ -f "${tmp_dir}/${BINARY}" ] || fatal "archive did not contain ${BINARY}"

mkdir -p "${INSTALL_DIR}"
install -m 755 "${tmp_dir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"

info "installed ${BINARY} v${version} to ${INSTALL_DIR}/${BINARY}"

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *) warn "${INSTALL_DIR} is not on your PATH" ;;
esac

if [ "$(uname -s)" = "Darwin" ]; then
  info "note: the binary is not notarized; if Gatekeeper blocks it, run: xattr -d com.apple.quarantine ${INSTALL_DIR}/${BINARY}"
fi
