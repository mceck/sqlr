#!/bin/sh
# Builds sqlr in release mode and installs it for the current user:
#
#   scripts/install.sh
#   PREFIX=/usr/local scripts/install.sh   # Linux: install under another prefix
#
# macOS: dist/sqlr.app is copied to /Applications.
# Linux: the binary goes to $PREFIX/bin, the icon and a desktop entry to $PREFIX/share
#        (PREFIX defaults to ~/.local).
# Elsewhere (Windows under MSYS/Git Bash, BSDs): `cargo install` into the cargo bin directory.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

case "$(uname -s)" in
Darwin)
    scripts/package-macos.sh
    rm -rf /Applications/sqlr.app
    cp -R dist/sqlr.app /Applications/
    echo "installed /Applications/sqlr.app"
    ;;
Linux)
    prefix=${PREFIX:-$HOME/.local}
    cargo build --release -p sqlr-app --bin sqlr
    install -d "$prefix/bin" "$prefix/share/applications" "$prefix/share/icons/hicolor/512x512/apps"
    install -m 755 target/release/sqlr "$prefix/bin/sqlr"
    install -m 644 docs/media/icon.png "$prefix/share/icons/hicolor/512x512/apps/sqlr.png"
    cat >"$prefix/share/applications/sqlr.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=sqlr
Comment=Lightweight, universal database client
Exec=$prefix/bin/sqlr
Icon=sqlr
Terminal=false
Categories=Development;Database;
DESKTOP
    echo "installed $prefix/bin/sqlr"
    case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "note: $prefix/bin is not on PATH" >&2 ;;
    esac
    ;;
*)
    cargo install --path crates/sqlr-app --bin sqlr --locked --force
    ;;
esac
