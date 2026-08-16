#!/usr/bin/env bash
# One-line installer for the modelcontextprotocol MCP servers binary.
#
# Detects the OS and CPU architecture, downloads the matching prebuilt
# binary from GitHub Releases, verifies its SHA-256 checksum, and installs
# it. Requires: curl, tar, and sha256sum (Linux/Android) or shasum (macOS).
#
# Usage:
#   curl -fsSL https://github.com/maxylev/modelcontextprotocol/releases/latest/download/install.sh | bash
#
# Override the install directory with INSTALL_DIR:
#   INSTALL_DIR=/usr/local/bin bash install.sh

set -euo pipefail

repo="maxylev/modelcontextprotocol"
base_url="https://github.com/${repo}/releases/latest/download"

die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

# --- detect OS ---
case "$(uname -s)" in
  Darwin) os="darwin" ;;
  Linux) os="linux" ;;
  *) die "unsupported operating system: $(uname -s)" ;;
esac

# Termux / Android: TERMUX_VERSION is set, or termux-info is on PATH.
if [ "${TERMUX_VERSION:-}" != "" ] || [ -x "${PREFIX:-}/bin/termux-info" ]; then
  os="android"
fi

# --- detect CPU architecture ---
machine="$(uname -m)"
case "${machine}" in
  x86_64 | amd64) arch="x86_64" ;;
  aarch64 | arm64) arch="aarch64" ;;
  armv7l | armhf) arch="armv7" ;;
  *) die "unsupported CPU architecture: ${machine}" ;;
esac

# --- pick the matching prebuilt binary ---
case "${os}:${arch}" in
  darwin:aarch64) target="aarch64-apple-darwin" ;;
  darwin:x86_64) target="x86_64-apple-darwin" ;;
  linux:x86_64) target="x86_64-unknown-linux-musl" ;;
  linux:aarch64) target="aarch64-unknown-linux-musl" ;;
  linux:armv7) target="armv7-unknown-linux-musleabihf" ;;
  android:aarch64) target="aarch64-linux-android" ;;
  android:x86_64) target="x86_64-linux-android" ;;
  android:armv7) target="armv7-linux-androideabi" ;;
  *) die "no prebuilt binary for ${os}/${arch}" ;;
esac

# --- install location ---
if [ "${os}" = "android" ]; then
  install_dir="${INSTALL_DIR:-${PREFIX}/bin}"
else
  install_dir="${INSTALL_DIR:-${HOME}/.local/bin}"
fi

# --- download, verify, install ---
base_name="modelcontextprotocol-${target}-rust-msrv"
archive_url="${base_url}/${base_name}.tar.gz"
checksum_url="${base_url}/${base_name}.sha256"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

printf 'Downloading %s\n' "${archive_url}"
curl -fsSL -o "${tmp}/${base_name}.tar.gz" "${archive_url}"
curl -fsSL -o "${tmp}/${base_name}.sha256" "${checksum_url}"

verify_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    return 1
  fi
}

if expected="$(verify_sha256 "${tmp}/${base_name}.tar.gz")"; then
  # The .sha256 file also covers the install.sh asset; check the archive line only.
  actual="$(grep -F "${base_name}.tar.gz" "${tmp}/${base_name}.sha256" | awk '{print $1}')"
  [ "${expected}" = "${actual}" ] || die "checksum mismatch for ${base_name}.tar.gz"
  printf 'Checksum verified\n'
else
  printf 'install.sh: warning: no sha256sum/shasum found; skipping checksum verification\n' >&2
fi

tar -xzf "${tmp}/${base_name}.tar.gz" -C "${tmp}"
mkdir -p "${install_dir}"
install -m 0755 "${tmp}/modelcontextprotocol" "${install_dir}/modelcontextprotocol"

printf 'Installed %s\n' "${install_dir}/modelcontextprotocol"
case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  # shellcheck disable=SC2016 # keep $PATH literal in the printed hint
  *) printf 'Add it to your PATH: export PATH="%s:$PATH"\n' "${install_dir}" ;;
esac
