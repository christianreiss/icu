#!/usr/bin/env bash
# Builds the portable Windows client into dist/<version>/:
# icu-<version>-windows-x86_64.zip (icu.exe, licenses, a short readme) and
# BUILDINFO.windows (commit, compiler, features, the ZIP), then rewrites
# SHA256SUMS over the BUILDINFO files there and what they list
# (packaging/checksums.sh).
#
#   packaging/windows/build.sh
#
# Environment:
#   ALLOW_DIRTY=1   build from a dirty working tree (never for a release:
#                   BUILDINFO says DIRTY, packaging/release.sh refuses it)
#   NO_AEC=1        build without the echo canceller (feature `aec`)
#
# Cross-built on Fedora for x86_64-pc-windows-gnu (Research/cross-compile.md):
# needs mingw64-gcc, mingw64-gcc-c++, mingw64-winpthreads-static,
# mingw64-binutils (windres for the icon and version resource),
# rust-std-static-x86_64-pc-windows-gnu, meson, ninja, zip, cargo-about
# 0.9.2 and cargo-deny 0.20.2 (same versions as the Linux build container). The exe is
# unsigned; SmartScreen warns on first start (Research/packaging-licensing.md).
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT=$PWD
TARGET=x86_64-pc-windows-gnu

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
[ -n "$VERSION" ] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }
OUT=dist/$VERSION
WORK=target/pkg/windows
NAME=icu-$VERSION-windows-x86_64

log() { printf '\n==> %s\n' "$*"; }
die() { echo "windows: $*" >&2; exit 1; }

if [ "${ALLOW_DIRTY:-0}" != 1 ] && [ -n "$(git status --porcelain)" ]; then
    die "working tree is dirty; commit first or set ALLOW_DIRTY=1"
fi
COMMIT=$(git rev-parse HEAD)

# openh264 asks for libstdc++ dynamically; this copy links it statically
# (build/win-static/README.md).
cp -u /usr/x86_64-w64-mingw32/sys-root/mingw/lib/libstdc++.a build/win-static/

features=gui,camera,tray,overlay
if [ "${NO_AEC:-0}" != 1 ]; then
    features=aec,$features
fi

log "cargo build --release --target $TARGET ($features)"
# build/win-aec/meson hands webrtc-audio-processing's meson the mingw cross
# file (Research/aec.md, A1).
PATH="$ROOT/build/win-aec:$PATH" cargo build --release --locked -p icu-client \
    --target "$TARGET" --no-default-features --features "$features"
EXE=target/$TARGET/release/icu.exe

log "imports: Windows system DLLs only"
# Everything a stock Windows 10/11 ships. mf*.dll are missing on N/KN
# editions without the Media Feature Pack (camera, Client/IMPLEMENTATION.md §1).
allowed='^(advapi32|api-ms-win-core-[a-z0-9-]+|bcrypt|bcryptprimitives|combase|comctl32|crypt32|d3d11|d3d12|dwmapi|dxgi|gdi32|imm32|kernel32|mf|mfplat|mfreadwrite|mmdevapi|msvcrt|ntdll|ole32|oleaut32|opengl32|propsys|rpcrt4|secur32|setupapi|shell32|shlwapi|uiautomationcore|user32|userenv|uxtheme|winmm|ws2_32|wtsapi32|avrt)\.dll$'
dlls=$(x86_64-w64-mingw32-objdump -p "$EXE" | awk '/DLL Name:/{print tolower($3)}' | sort -u)
bad=$(echo "$dlls" | grep -Ev "$allowed" || true)
[ -z "$bad" ] || die "icu.exe imports non-system DLLs: $bad"
echo "$dlls" | tr '\n' ' '; echo

log "resources: icon, version and manifest"
# crates/icu-client/build.rs only warns when windres is missing; a release
# without them shows a blank icon in Explorer and for .icu files.
x86_64-w64-mingw32-objdump -h "$EXE" | grep -q '\.rsrc' \
    || die "icu.exe has no resource section (x86_64-w64-mingw32-windres missing?)"

log "supply chain and Windows third-party licenses"
cargo deny --locked check
mkdir -p "$WORK"
cargo about generate --locked --fail --target "$TARGET" \
    --manifest-path crates/icu-client/Cargo.toml --no-default-features \
    --features "$features" packaging/about.hbs -o "$WORK/THIRD-PARTY-LICENSES.txt"
{
    printf '\n%s\n%s\nUsed by:\n  - Inter (embedded)\n\n' \
        '------------------------------------------------------------------------------' \
        'SIL Open Font License 1.1 (OFL-1.1)'
    cat crates/icu-client/assets/fonts/inter/LICENSE.txt
} >> "$WORK/THIRD-PARTY-LICENSES.txt"
for dependency in windows opusic-sys openh264; do
    grep -q "$dependency" "$WORK/THIRD-PARTY-LICENSES.txt" || die "license missing: $dependency"
done
if [ "${NO_AEC:-0}" != 1 ]; then
    grep -q webrtc-audio-processing "$WORK/THIRD-PARTY-LICENSES.txt" || die "AEC license missing"
fi

log "zip"
rm -rf "$WORK/$NAME" && mkdir -p "$WORK/$NAME" "$OUT"
cp "$EXE" LICENSE "$WORK/$NAME/"
cp "$WORK/THIRD-PARTY-LICENSES.txt" "$WORK/$NAME/THIRD-PARTY-LICENSES.txt"
sed "s/@VERSION@/$VERSION/" packaging/windows/README.txt | sed 's/$/\r/' > "$WORK/$NAME/LIESMICH.txt"
(cd "$WORK" && rm -f "$ROOT/$OUT/$NAME.zip" && zip -qr9 "$ROOT/$OUT/$NAME.zip" "$NAME")

{
    echo "commit $COMMIT"
    if [ "${ALLOW_DIRTY:-0}" = 1 ]; then echo DIRTY; fi
    echo "rustc windows: $(rustc -V)"
    echo "features $features"
    echo "artifact $NAME.zip"
} > "$OUT/BUILDINFO.windows"
packaging/checksums.sh "$OUT"
log "done: $OUT/$NAME.zip"
ls -l "$OUT/$NAME.zip"
