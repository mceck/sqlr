#!/bin/sh
# Packs a sidecar driver into the archive the Driver Manager installs (Install from file…, or
# a download from the release, packed by scripts/package-release-drivers.sh):
#
#   scripts/package-driver.sh hana            out.tar.gz [--bin PATH]
#   scripts/package-driver.sh duckdb          out.tar.gz [--bin PATH]
#   scripts/package-driver.sh jdbc            out.tar.gz [--bin PATH] [--bridge PATH]
#   scripts/package-driver.sh jdbc-h2         out.tar.gz [--jar PATH ...]
#
# `jdbc` and `jdbc-<database>` are presets of the JDBC bridge (see crates/sqlr-driver-jdbc);
# --jar copies JDBC driver jars into the archive's jars/ directory. Without --bin the driver is
# built in release mode. The archive holds driver.toml, the executable and, for JDBC, the
# bridge jar.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
id=${1:?usage: package-driver.sh <hana|duckdb|jdbc|jdbc-NAME> <out.tar.gz> [--bin PATH] [--bridge PATH] [--jar PATH]...}
out=${2:?missing output file}
shift 2
bin=""
bridge=""
jars=""
while [ $# -gt 0 ]; do
    case "$1" in
        --bin) bin=$2; shift 2 ;;
        --bridge) bridge=$2; shift 2 ;;
        --jar) jars="$jars $2"; shift 2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done

case "$id" in
    hana|duckdb) package=sqlr-driver-$id; dir=drivers/$package; args='[]' ;;
    jdbc|jdbc-*) package=sqlr-driver-jdbc; dir=crates/$package; args="[\"--preset\", \"$id\"]" ;;
    *) echo "unknown driver $id" >&2; exit 2 ;;
esac
case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) exe=$package.exe ;;
    *) exe=$package ;;
esac
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/$dir/Cargo.toml" | head -1)

if [ -z "$bin" ]; then
    (cd "$root" && cargo build --release -p "$package")
    bin="$root/target/release/$exe"
fi
[ -f "$bin" ] || { echo "no executable at $bin" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp "$bin" "$work/$exe"
chmod 755 "$work/$exe"
printf 'id = "%s"\nversion = "%s"\nexecutable = "%s"\nargs = %s\n' "$id" "$version" "$exe" "$args" > "$work/driver.toml"
if [ "$package" = sqlr-driver-jdbc ]; then
    if [ -z "$bridge" ]; then
        bridge="$work/sqlr-jdbc-bridge.jar"
        "$root/drivers/jdbc-bridge/build.sh" "$bridge" > /dev/null
    else
        cp "$bridge" "$work/sqlr-jdbc-bridge.jar"
    fi
fi
if [ -n "$jars" ]; then
    mkdir "$work/jars"
    for jar in $jars; do cp "$jar" "$work/jars/"; done
fi
mkdir -p "$(dirname "$out")"
# No AppleDouble (._*) entries for extended attributes on macOS.
(cd "$work" && COPYFILE_DISABLE=1 tar -czf - .) > "$out"
echo "$out"
