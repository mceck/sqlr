#!/bin/sh
# Builds the JDBC bridge with javac and jar (Java 17+, no build tool).
#   build.sh [output.jar]      default: ./sqlr-jdbc-bridge.jar
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$here/sqlr-jdbc-bridge.jar}
javac_bin=javac
jar_bin=jar
if [ -n "${JAVA_HOME:-}" ] && [ -x "$JAVA_HOME/bin/javac" ]; then
    javac_bin="$JAVA_HOME/bin/javac"
    jar_bin="$JAVA_HOME/bin/jar"
fi
classes=$(mktemp -d)
trap 'rm -rf "$classes"' EXIT
"$javac_bin" --release 17 -Xlint:all -Werror -d "$classes" "$here"/java/dev/sqlr/jdbc/*.java
printf 'Main-Class: dev.sqlr.jdbc.Main\n' > "$classes/manifest.txt"
mkdir -p "$(dirname "$out")"
"$jar_bin" --create --file "$out" --manifest "$classes/manifest.txt" -C "$classes" dev
echo "$out"
