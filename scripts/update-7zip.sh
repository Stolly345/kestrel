#!/bin/sh
# Fetch the official static 7-Zip into packaging/7zip/, verified against the GitHub release
# digest (7-zip.org publishes no signatures). Usage: scripts/update-7zip.sh 26.05
# Check https://github.com/ip7z/7zip/releases monthly: most releases fix decoder CVEs.
set -eu
v=${1:?version, e.g. 26.05}
tag=$(echo "$v" | tr -d .)
name=7z$tag-linux-x64.tar.xz
want=$(gh api "repos/ip7z/7zip/releases/tags/$v" --jq ".assets[] | select(.name==\"$name\") | .digest" | sed 's/^sha256://')
[ -n "$want" ] || { echo "no digest for $name" >&2; exit 1; }
out=$(cd "$(dirname "$0")/../packaging/7zip" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/$name" "https://github.com/ip7z/7zip/releases/download/$v/$name"
echo "$want  $tmp/$name" | sha256sum -c -
tar -xJf "$tmp/$name" -C "$tmp" 7zzs License.txt
install -m 755 "$tmp/7zzs" "$out/7zz"
install -m 644 "$tmp/License.txt" "$out/License.txt"
echo "$v $want" > "$out/VERSION"
echo "7-Zip $v -> $out"
