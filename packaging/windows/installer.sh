#!/usr/bin/env bash
# Builds the NSIS installer into dist/<version>/:
# icu-<version>-windows-x86_64-setup.exe, alongside (not instead of) the
# portable ZIP packaging/windows/build.sh already produces there. Reuses
# build.sh's staged files byte-for-byte (same icu.exe, LICENSE,
# THIRD-PARTY-LICENSES.txt that went into the ZIP) rather than rebuilding
# anything, then writes its own BUILDINFO.windows-installer and rewrites
# SHA256SUMS over dist/<version>/ (packaging/checksums.sh).
#
# Run after packaging/windows/build.sh, on the same commit:
#   packaging/windows/build.sh && packaging/windows/installer.sh
#
# Needs makensis (Fedora: `sudo dnf install mingw64-nsis mingw32-nsis`,
# NSIS 3.11-2.fc43 pinned, same versions as the Linux build container).
# mingw64-nsis (+ mingw-nsis-base) provides the amd64-unicode stub the
# installer actually ships with (`Target amd64-unicode` in installer.nsi);
# mingw32-nsis is also required even though nothing x86 is built — makensis
# initializes an x86-unicode default stub before it even reads the script
# (verified: it fails the same way on `-VERSION`/`-HELP` with no script at
# all), and Fedora splits that stub into the separate x86 package.
#
# Environment:
#   ALLOW_DIRTY=1   build from a dirty working tree (never for a release:
#                   BUILDINFO says DIRTY, packaging/release.sh refuses it)
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT=$PWD
NSI=packaging/windows/installer.nsi
NOTICE=packaging/windows/notices/nsis-COPYING
ICON=crates/icu-client/assets/icu.ico

log() { printf '\n==> %s\n' "$*"; }
die() { echo "windows installer: $*" >&2; exit 1; }

command -v makensis >/dev/null || die "makensis not found (sudo dnf install mingw64-nsis mingw32-nsis)"

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
[ -n "$VERSION" ] || die "cannot read version from Cargo.toml"
OUT=dist/$VERSION
WORK=target/pkg/windows
NAME=icu-$VERSION-windows-x86_64
STAGE=$WORK/$NAME
OUTEXE=$OUT/$NAME-setup.exe

if [ "${ALLOW_DIRTY:-0}" != 1 ] && [ -n "$(git status --porcelain)" ]; then
    die "working tree is dirty; commit first or set ALLOW_DIRTY=1"
fi
COMMIT=$(git rev-parse HEAD)

[ -f "$STAGE/icu.exe" ] && [ -f "$STAGE/LICENSE" ] && [ -f "$STAGE/THIRD-PARTY-LICENSES.txt" ] \
    || die "$STAGE is missing staged files; run packaging/windows/build.sh first"
[ -f "$OUT/BUILDINFO.windows" ] || die "$OUT/BUILDINFO.windows missing; run packaging/windows/build.sh first"
zip_commit=$(sed -n 's/^commit //p' "$OUT/BUILDINFO.windows")
[ "$zip_commit" = "$COMMIT" ] \
    || die "$OUT/BUILDINFO.windows is from $zip_commit, not HEAD ($COMMIT); rebuild with packaging/windows/build.sh"
if grep -qx DIRTY "$OUT/BUILDINFO.windows" && [ "${ALLOW_DIRTY:-0}" != 1 ]; then
    die "$OUT/BUILDINFO.windows was built DIRTY; rebuild clean or set ALLOW_DIRTY=1 here too"
fi
[ -f "$NOTICE" ] || die "$NOTICE missing (NSIS stub license text, see packaging/windows/build.sh's notice() pattern)"

# VIProductVersion needs four numeric parts (crates/icu-client/build.rs
# does the same split for the exe's own version resource: major.minor.patch
# then a literal 0; a pre-release suffix like -beta.2 has no room there).
IFS='.' read -r major minor patch _ <<<"$(echo "$VERSION" | tr -- '-+' '.')"
VIVERSION="${major:-0}.${minor:-0}.${patch:-0}.0"

log "makensis"
mkdir -p "$OUT"
makensis -WX -V2 -INPUTCHARSET UTF8 -NOCD \
    -DVERSION="$VERSION" -DVIVERSION="$VIVERSION" \
    -DSTAGE="$ROOT/$STAGE" -DOUTFILE="$ROOT/$OUTEXE" \
    -DICON="$ROOT/$ICON" -DNOTICE="$ROOT/$NOTICE" \
    "$NSI"
[ -f "$OUTEXE" ] || die "makensis did not produce $OUTEXE"

log "sanity: PE binary"
# Target amd64-unicode in installer.nsi selects the 64-bit stub; `file`
# reports "PE32+ executable ... x86-64" (verified on this host).
file "$OUTEXE" | grep -qi 'PE32' || die "$OUTEXE is not a PE executable"

{
    echo "commit $COMMIT"
    if [ "${ALLOW_DIRTY:-0}" = 1 ]; then echo DIRTY; fi
    echo "makensis: $(makensis -VERSION 2>&1 || true)"
    echo "artifact $NAME-setup.exe"
} > "$OUT/BUILDINFO.windows-installer"
packaging/checksums.sh "$OUT"
log "done: $OUTEXE"
ls -l "$OUTEXE"
