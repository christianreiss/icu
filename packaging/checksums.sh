#!/usr/bin/env bash
# The release files of one dist/<version>/: every BUILDINFO.<component>
# there and the artifacts it lists (`artifact <file>` lines), nothing else.
# A package left over from an older build or release iteration is not
# listed, so it is neither checksummed nor published.
#
#   packaging/checksums.sh dist/<version>          # rewrite SHA256SUMS
#   packaging/checksums.sh --list dist/<version>   # print the file names
#
# packaging/build.sh and packaging/windows/build.sh write their BUILDINFO
# and then call this; packaging/release.sh publishes exactly the --list.
set -euo pipefail

list=0
if [ "${1:-}" = --list ]; then
    list=1
    shift
fi
dir=${1:?usage: checksums.sh [--list] dist/<version>}
cd "$dir"

compgen -G 'BUILDINFO.*' >/dev/null || { echo "checksums: no BUILDINFO.* in $dir" >&2; exit 1; }
mapfile -t files < <({ printf '%s\n' BUILDINFO.*; sed -n 's/^artifact //p' BUILDINFO.*; } | LC_ALL=C sort -u)
for f in "${files[@]}"; do
    [ -f "$f" ] || { echo "checksums: $dir/$f is listed but missing" >&2; exit 1; }
done

if [ "$list" = 1 ]; then
    printf '%s\n' "${files[@]}"
else
    sha256sum -- "${files[@]}" > SHA256SUMS.new
    mv SHA256SUMS.new SHA256SUMS
fi
