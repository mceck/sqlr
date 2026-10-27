#!/bin/sh
# Sets the version of every first-party crate to the same value, so they stay aligned:
#
#   scripts/bump-version.sh 0.2.0
#   scripts/bump-version.sh patch|minor|major   # relative to the sqlr-app version
#
# Touches crates/*, drivers/*, tools/* and the driver template (the fuzz package stays 0.0.0)
# and the `metadata.sqlr-version` of every sqlr skill, then refreshes Cargo.lock. It does
# not commit.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
arg=${1:?usage: bump-version.sh <X.Y.Z|patch|minor|major>}

current=$(sed -n 's/^version = "\(.*\)"$/\1/p' crates/sqlr-app/Cargo.toml | head -n 1)
case "$arg" in
patch | minor | major)
    major=${current%%.*}
    rest=${current#*.}
    minor=${rest%%.*}
    patch=${rest#*.}
    case "$arg" in
    major) new="$((major + 1)).0.0" ;;
    minor) new="$major.$((minor + 1)).0" ;;
    patch) new="$major.$minor.$((patch + 1))" ;;
    esac
    ;;
*)
    echo "$arg" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' || {
        echo "invalid version: $arg" >&2
        exit 1
    }
    new=$arg
    ;;
esac

for manifest in crates/*/Cargo.toml drivers/*/Cargo.toml tools/*/Cargo.toml templates/*/Cargo.toml; do
    # Only the first `version = "…"` line, which is the [package] one.
    awk -v v="$new" '!done && /^version = "/ { print "version = \"" v "\""; done = 1; next } { print }' \
        "$manifest" >"$manifest.tmp"
    mv "$manifest.tmp" "$manifest"
done

for skill in skills/*/SKILL.md; do
    sed "s/^  sqlr-version: .*$/  sqlr-version: $new/" "$skill" >"$skill.tmp"
    mv "$skill.tmp" "$skill"
done

cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null
echo "bumped $current -> $new"
