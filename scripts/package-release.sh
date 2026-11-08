#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
version=${1:?usage: package-release.sh <version> <target>}
target=${2:?usage: package-release.sh <version> <target>}
package="keyboard-debouncer-v$version-$target"
destination="dist/$package"

rm -rf -- "$destination"
mkdir -p "$destination/scripts"
install -m 755 "target/$target/release/keyboard-debouncer" "$destination/keyboard-debouncer"
install -m 755 install.sh uninstall.sh "$destination/"
install -m 644 keyboard-debouncer.service debouncer.conf.example package.json README.md CHANGELOG.md LICENSE "$destination/"
install -m 644 scripts/analyze.ts scripts/timing.ts "$destination/scripts/"
install -m 755 scripts/report.sh "$destination/scripts/"
cp -R docs "$destination/"

cd dist
archive="$package.tar.gz"
tar -czf "$archive" "$package"
sha256sum "$archive" > "$archive.sha256"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf 'archive=%s\n' "$archive" >> "$GITHUB_OUTPUT"
fi
