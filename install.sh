#!/usr/bin/env bash
# Hermes Desktop installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/j4ckxyz/hermes-desktop/main/install.sh | bash
#
# Downloads the latest release (or HERMES_VERSION, e.g. v0.6.1) from GitHub Releases, checks it
# against the release's SHA256SUMS, and installs:
#   macOS  Hermes.app into /Applications (or ~/Applications if that isn't writable), with the
#          download quarantine flag cleared.
#   Linux  ~/.local/bin/hermes-desktop, plus a menu entry and icon under ~/.local/share.
# Nothing runs as root and nothing is launched.
#
# Options (environment): HERMES_VERSION, HERMES_BIN_DIR (Linux, default ~/.local/bin),
# HERMES_APP_DIR (macOS, default /Applications), HERMES_BASE_URL (a mirror of the release assets).
set -euo pipefail

REPO="j4ckxyz/hermes-desktop"
VERSION="${HERMES_VERSION:-latest}"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) die "unsupported system $(uname -s); see https://github.com/$REPO#install" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) die "unsupported CPU $(uname -m)" ;;
esac

if [ "$os" = macos ]; then
  asset="hermes-desktop-macos-universal.zip"
else
  asset="hermes-desktop-linux-$arch.tar.gz"
fi

if [ -n "${HERMES_BASE_URL:-}" ]; then
  base="$HERMES_BASE_URL" # a mirror, or a local directory served over HTTP for testing
elif [ "$VERSION" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

command -v curl >/dev/null || die "curl is required"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "Downloading $asset ($VERSION)"
curl -fSL --progress-bar --retry 3 -o "$tmp/$asset" "$base/$asset" || die "download failed: $base/$asset"
curl -fsSL --retry 3 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "could not download SHA256SUMS"

if [ -n "${HERMES_TEST_CORRUPT:-}" ]; then printf 'x' >> "$tmp/$asset"; fi # CI: prove a bad file is refused

expected="$(awk -v f="$asset" '$2 == f || $2 == "*" f {print $1}' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || die "$asset is not listed in SHA256SUMS"
if command -v sha256sum >/dev/null; then
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
fi
[ "$actual" = "$expected" ] || die "checksum mismatch for $asset (expected $expected, got $actual); nothing was installed"
say "Checksum verified"

if [ "$os" = macos ]; then
  dest="${HERMES_APP_DIR:-/Applications}"
  if ! [ -w "$dest" ]; then
    dest="$HOME/Applications"
    mkdir -p "$dest"
  fi
  ditto -x -k "$tmp/$asset" "$tmp/unpacked"
  [ -d "$tmp/unpacked/Hermes.app" ] || die "archive has no Hermes.app"
  rm -rf "$dest/Hermes.app"
  ditto "$tmp/unpacked/Hermes.app" "$dest/Hermes.app"
  # The app is ad-hoc signed (no paid Apple Developer ID), so clear the download quarantine flag
  # that would otherwise make Gatekeeper refuse to open it.
  xattr -dr com.apple.quarantine "$dest/Hermes.app" 2>/dev/null || true
  bin="$dest/Hermes.app/Contents/MacOS/hermes-desktop"
  say "Installed $("$bin" --version) to $dest/Hermes.app"
  say "Open it from Launchpad or Spotlight (\"Hermes\")."
else
  bindir="${HERMES_BIN_DIR:-$HOME/.local/bin}"
  share="${XDG_DATA_HOME:-$HOME/.local/share}"
  mkdir -p "$bindir" "$share/applications" "$share/icons/hicolor/256x256/apps"
  tar -xzf "$tmp/$asset" -C "$tmp"
  install -m 755 "$tmp/hermes-desktop/hermes-desktop" "$bindir/hermes-desktop"
  install -m 644 "$tmp/hermes-desktop/hermes-desktop.png" "$share/icons/hicolor/256x256/apps/hermes-desktop.png"
  sed "s|^Exec=.*|Exec=$bindir/hermes-desktop|" "$tmp/hermes-desktop/hermes-desktop.desktop" > "$share/applications/hermes-desktop.desktop"
  command -v update-desktop-database >/dev/null && update-desktop-database "$share/applications" >/dev/null 2>&1 || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$share/icons/hicolor" >/dev/null 2>&1 || true
  if version="$("$bindir/hermes-desktop" --version 2>/dev/null)"; then
    say "Installed $version to $bindir/hermes-desktop"
  else
    say "Installed to $bindir/hermes-desktop"
    printf '    It needs a few system libraries to start; on Debian/Ubuntu:\n'
    printf '    sudo apt install libxkbcommon-x11-0 libvulkan1 libwayland-client0 libfontconfig1\n'
  fi
  case ":$PATH:" in
    *":$bindir:"*) ;;
    *) printf '    Add %s to your PATH to run hermes-desktop from a terminal.\n' "$bindir" ;;
  esac
  say "Hermes is in your applications menu."
fi
