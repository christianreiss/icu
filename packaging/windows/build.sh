#!/usr/bin/env bash
# Builds the portable Windows client into dist/<version>/:
# icu-<version>-windows-x86_64.zip (icu.exe, licenses, a short readme),
# then rewrites SHA256SUMS over everything in dist/<version>/.
#
#   packaging/windows/build.sh
#
# Environment:
#   ALLOW_DIRTY=1   build from a dirty working tree (never for a release)
#   NO_AEC=1        build without the echo canceller (feature `aec`)
#
# Cross-built on Fedora for x86_64-pc-windows-gnu (Research/cross-compile.md):
# needs mingw64-gcc, mingw64-gcc-c++, mingw64-winpthreads-static,
# rust-std-static-x86_64-pc-windows-gnu, meson, ninja and zip. The exe is
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
allowed='^(advapi32|api-ms-win-core-[a-z0-9-]+|bcrypt|bcryptprimitives|combase|comctl32|crypt32|d3d11|d3d12|dwmapi|dxgi|gdi32|imm32|kernel32|mf|mfplat|mfreadwrite|mmdevapi|msvcrt|ntdll|ole32|oleaut32|opengl32|propsys|rpcrt4|secur32|setupapi|shell32|shlwapi|uiautomationcore|user32|userenv|uxtheme|winmm|ws2_32|avrt)\.dll$'
dlls=$(x86_64-w64-mingw32-objdump -p "$EXE" | awk '/DLL Name:/{print tolower($3)}' | sort -u)
bad=$(echo "$dlls" | grep -Ev "$allowed" || true)
[ -z "$bad" ] || die "icu.exe imports non-system DLLs: $bad"
echo "$dlls" | tr '\n' ' '; echo

log "zip"
rm -rf "$WORK" && mkdir -p "$WORK/$NAME" "$OUT"
cp "$EXE" LICENSE "$WORK/$NAME/"
if [ -f target/pkg/THIRD-PARTY-LICENSES ]; then
    cp target/pkg/THIRD-PARTY-LICENSES "$WORK/$NAME/THIRD-PARTY-LICENSES.txt"
fi
sed "s/@VERSION@/$VERSION/" packaging/windows/README.txt | sed 's/$/\r/' > "$WORK/$NAME/LIESMICH.txt"
(cd "$WORK" && rm -f "$ROOT/$OUT/$NAME.zip" && zip -qr9 "$ROOT/$OUT/$NAME.zip" "$NAME")

(cd "$OUT" && rm -f SHA256SUMS && sha256sum -- $(ls *.rpm *.deb *.zip 2>/dev/null) > SHA256SUMS)
log "done: $OUT/$NAME.zip"
ls -l "$OUT/$NAME.zip"
