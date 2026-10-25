#!/bin/sh
# Downloads the latest sqlr release from GitHub for this system and installs it.
#
#   curl -fsSL https://raw.githubusercontent.com/mceck/sqlr/main/scripts/get-sqlr.sh | sh
#
# Linux:  installs bin/ and share/ from the tarball under ~/.local (no root needed).
# macOS:  copies sqlr.app from the dmg into /Applications, or ~/Applications when
#         /Applications is not writable.
#
# Environment:
#   SQLR_VERSION  install this version (for example 0.2.0) instead of the latest
#   SQLR_PREFIX   Linux install prefix (default ~/.local; use /usr/local with sudo)
set -eu

repo="mceck/sqlr"

say() { printf '%s\n' "$*"; }
fail() { printf 'get-sqlr: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "$1 is required but not installed"; }

need curl
need tar

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=macos; [ "$arch" = aarch64 ] && arch=arm64 ;;
  *) fail "unsupported system: $(uname -s). Windows builds are on https://github.com/$repo/releases/latest" ;;
esac

if [ -n "${SQLR_VERSION:-}" ]; then
  version=${SQLR_VERSION#v}
else
  # The /releases/latest redirect names the newest tag and, unlike the API, has no rate limit.
  url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") \
    || fail "could not look up the latest release"
  version=${url##*/v}
  [ -n "$version" ] && [ "$version" != "$url" ] || fail "could not read the latest version from $url"
fi

if [ "$os" = linux ]; then ext=tar.gz; else ext=dmg; fi
asset="sqlr_${version}_${os}-${arch}.${ext}"
base="https://github.com/$repo/releases/download/v${version}"

tmp=$(mktemp -d)
mounted=
cleanup() {
  [ -z "$mounted" ] || hdiutil detach -quiet "$mounted" >/dev/null 2>&1 || true
  rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

say "Downloading $asset"
curl -fSL --progress-bar -o "$tmp/$asset" "$base/$asset" \
  || fail "download failed: $base/$asset (is there a $os $arch build for v$version?)"

if curl -fsSL -o "$tmp/SHA256SUMS.txt" "$base/SHA256SUMS.txt"; then
  expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS.txt")
  [ -n "$expected" ] || fail "$asset is not listed in SHA256SUMS.txt"
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$asset" | awk '{ print $1 }')
  else
    actual=$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')
  fi
  [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"
  say "Checksum verified"
else
  say "No SHA256SUMS.txt for this release: skipping checksum verification"
fi

if [ "$os" = linux ]; then
  prefix=${SQLR_PREFIX:-$HOME/.local}
  tar -xzf "$tmp/$asset" -C "$tmp"
  stage="$tmp/sqlr_${version}_linux-${arch}"
  mkdir -p "$prefix"
  cp -R "$stage/bin" "$stage/share" "$prefix/"
  say "Installed sqlr $version to $prefix/bin/sqlr"
  case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) say "Add $prefix/bin to your PATH to run it as 'sqlr'." ;;
  esac
else
  need hdiutil
  mounted="$tmp/mount"
  mkdir -p "$mounted"
  hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mounted" "$tmp/$asset" >/dev/null
  dest=/Applications
  [ -w "$dest" ] || { dest="$HOME/Applications"; mkdir -p "$dest"; }
  rm -rf "$dest/sqlr.app"
  cp -R "$mounted/sqlr.app" "$dest/"
  # Releases are ad-hoc signed, not notarized; drop the quarantine flag set by the download.
  xattr -dr com.apple.quarantine "$dest/sqlr.app" 2>/dev/null || true
  say "Installed sqlr $version to $dest/sqlr.app"
fi
