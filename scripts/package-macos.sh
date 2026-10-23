#!/bin/sh
# Builds sqlr.app: a double-clickable macOS application bundle with the app icon.
#
#   scripts/package-macos.sh
#   scripts/package-macos.sh --bin target/release/sqlr
#   scripts/package-macos.sh --sign "Developer ID Application: Name (TEAMID)"
#
# Without --bin the app is built in release mode. The bundle is ad-hoc signed by
# default, which is enough to run it on this Mac; distribution to other Macs needs
# a Developer ID signature and notarization.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/dist/sqlr.app"
bin=""
identity="-"
while [ $# -gt 0 ]; do
    case "$1" in
        --bin) bin=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        --sign) identity=$2; shift 2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done

if [ -z "$bin" ]; then
    (cd "$root" && cargo build --release -p sqlr-app --bin sqlr)
    bin="$root/target/release/sqlr"
fi
[ -x "$bin" ] || { echo "no executable at $bin" >&2; exit 1; }

version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/crates/sqlr-app/Cargo.toml" | head -1)
icon="$root/docs/media/icon.png"
[ -f "$icon" ] || { echo "no icon at $icon" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out/Contents/MacOS" "$out/Contents/Resources"
cp "$bin" "$out/Contents/MacOS/sqlr"
chmod 755 "$out/Contents/MacOS/sqlr"
printf 'APPL????' > "$out/Contents/PkgInfo"

# The .icns from the single 1024 px artwork: every size macOS asks for.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
iconset="$work/sqlr.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$icon" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$icon" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$out/Contents/Resources/sqlr.icns"

cat > "$out/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>sqlr</string>
    <key>CFBundleExecutable</key>
    <string>sqlr</string>
    <key>CFBundleIconFile</key>
    <string>sqlr.icns</string>
    <key>CFBundleIdentifier</key>
    <string>dev.sqlr.sqlr</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>sqlr</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
EOF
plutil -lint "$out/Contents/Info.plist" >/dev/null

# Signing the bundle after copying the binary keeps the sealed resources valid.
codesign --force --sign "$identity" "$out"
codesign --verify --strict "$out"

echo "built $out"
echo "run:     open \"$out\""
echo "install: drag it to /Applications, or: cp -R \"$out\" /Applications/"
