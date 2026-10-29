#!/bin/sh
# Builds and packs every sidecar driver a sqlr release publishes, for one platform:
#
#   scripts/package-release-drivers.sh OUT_DIR [--target TRIPLE] [--platform KEY]
#
# writes OUT_DIR/sqlr-driver-<id>_<version>_<platform>.tar.gz for each id in DRIVERS, the
# names the Driver Manager downloads from the release (sqlr_registry::ReleaseSource). KEY is
# `<os>-<arch>` as Rust names them (macos-aarch64, linux-x86_64, windows-x86_64, …); by default
# it is this machine's, or the --target's architecture on this OS. The JDBC bridge needs a JDK
# (javac and jar, Java 17+).
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
# The drivers of sqlr_core's known databases that do not ship with the app.
DRIVERS="hana duckdb mongodb jdbc jdbc-db2 jdbc-h2"

out=${1:?usage: package-release-drivers.sh OUT_DIR [--target TRIPLE] [--platform KEY]}
shift
target=""
platform=""
while [ $# -gt 0 ]; do
    case "$1" in
        --target) target=$2; shift 2 ;;
        --platform) platform=$2; shift 2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done

ext=""
case "$(uname -s)" in
    Darwin) os=macos ;;
    Linux) os=linux ;;
    MINGW* | MSYS* | CYGWIN*) os=windows; ext=.exe ;;
    *) echo "unsupported OS $(uname -s)" >&2; exit 2 ;;
esac
if [ -z "$platform" ]; then
    if [ -n "$target" ]; then
        arch=${target%%-*}
    else
        arch=$(uname -m)
    fi
    case "$arch" in
        arm64) arch=aarch64 ;;
        amd64) arch=x86_64 ;;
    esac
    platform=$os-$arch
fi

crate_version() {
    sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/$1/Cargo.toml" | head -1
}
version=$(crate_version crates/sqlr-app)
for dir in drivers/sqlr-driver-hana drivers/sqlr-driver-duckdb drivers/sqlr-driver-mongo crates/sqlr-driver-jdbc; do
    if [ "$(crate_version "$dir")" != "$version" ]; then
        echo "$dir is not at the release version $version (scripts/bump-version.sh)" >&2
        exit 1
    fi
done

(cd "$root" && cargo build --release ${target:+--target "$target"} \
    -p sqlr-driver-hana -p sqlr-driver-duckdb -p sqlr-driver-mongo -p sqlr-driver-jdbc)
bindir="$root/target/${target:+$target/}release"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
sh "$root/drivers/jdbc-bridge/build.sh" "$work/sqlr-jdbc-bridge.jar" > /dev/null

mkdir -p "$out"
for id in $DRIVERS; do
    case "$id" in
        mongodb) package=sqlr-driver-mongodb ;;
        jdbc | jdbc-*) package=sqlr-driver-jdbc ;;
        *) package=sqlr-driver-$id ;;
    esac
    sh "$root/scripts/package-driver.sh" "$id" "$out/sqlr-driver-${id}_${version}_${platform}.tar.gz" \
        --bin "$bindir/$package$ext" --bridge "$work/sqlr-jdbc-bridge.jar"
done
