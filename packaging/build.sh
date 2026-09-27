#!/usr/bin/env bash
# Builds the icu-server and icu (client) rpm/deb packages into dist/<version>/,
# with BUILDINFO.linux (commit, compilers, the packages of this run) and
# SHA256SUMS over the BUILDINFO files there and what they list
# (packaging/checksums.sh): packages of an earlier run or release iteration
# stay in the directory but are neither listed nor published.
#
#   packaging/build.sh                 # everything, then smoke tests
#   DISTS="fc43" packaging/build.sh    # subset of client distros
#
# Environment:
#   RELEASE=1          package release (iteration)
#   DISTS="fc42 fc43 debian13"        client distros (smoke-tested with the server)
#   SERVER_DISTS="debian12 alma10"    smoke-tested with the server package only
#   SKIP_TESTS=1       skip `cargo test --workspace` on the host
#   SKIP_SMOKE=1       skip packaging/smoke.sh
#   ALLOW_DIRTY=1      build from a dirty working tree (never for a release:
#                      BUILDINFO says DIRTY, packaging/release.sh refuses it)
#
# Needs only podman on the host (plus cargo for the tests). The server is one
# static musl binary for both formats; the client is built per distro because
# it links pipewire, libstdc++ and the distro's abseil.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$PWD

RELEASE=${RELEASE:-1}
DISTS=${DISTS:-"fc42 fc43 debian13"}
RUST_ALPINE=docker.io/library/rust:1.98.1-alpine
NFPM_IMAGE=docker.io/goreleaser/nfpm:v2.47.0
CARGO_CACHE=icu-cargo-cache

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
[ -n "$VERSION" ] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }
OUT=dist/$VERSION
WORK=target/pkg
# nfpm writes here first, so this run's packages are known by name.
STAGE=$WORK/out

log() { printf '\n==> %s\n' "$*"; }

# Label relabeling (:Z) of the whole repo is slow and races between
# containers; disable SELinux separation for these throwaway build containers.
run() {
    podman run --rm --security-opt label=disable \
        -v "$ROOT":/src -w /src \
        -v "$CARGO_CACHE":/cargo -e CARGO_HOME=/cargo \
        "$@"
}

dist_image()  { echo "icu-build-$1"; }
dist_format() { case $1 in fc*) echo rpm ;; debian*) echo deb ;; *) return 1 ;; esac; }
dist_release() {
    case $1 in
        fc*) echo "$RELEASE.$1" ;;
        debian*) echo "$RELEASE+deb${1#debian}" ;;
    esac
}

if [ "${ALLOW_DIRTY:-0}" != 1 ] && [ -n "$(git status --porcelain)" ]; then
    echo "working tree is dirty; commit first or set ALLOW_DIRTY=1" >&2
    exit 1
fi

COMMIT=$(git rev-parse HEAD)

mkdir -p "$OUT" "$WORK"
rm -rf "$STAGE" "$WORK"/rustc-*
mkdir -p "$STAGE"
podman volume exists "$CARGO_CACHE" || podman volume create "$CARGO_CACHE" >/dev/null

if [ "${SKIP_TESTS:-0}" != 1 ]; then
    log "cargo test --workspace"
    cargo test --workspace --locked
fi

log "server: static musl binary"
# Profile release-server: release with panic = "unwind", so a panic ends one
# connection instead of the whole server (Cargo.toml).
run -e CARGO_TARGET_DIR=/src/target/musl "$RUST_ALPINE" sh -euc '
    apk add --no-cache musl-dev >/dev/null
    rustc -V > target/pkg/rustc-server
    cargo build --profile release-server --locked -p icu-server --target x86_64-unknown-linux-musl'
SERVER_BIN=target/musl/x86_64-unknown-linux-musl/release-server/icu-server
file "$SERVER_BIN" | grep -q 'static-pie linked' \
    || { echo "$SERVER_BIN is not static-pie" >&2; exit 1; }

for d in $DISTS; do
    log "client: build image for $d"
    case $d in
        fc*) podman build -q -f packaging/containers/client-fedora.Containerfile \
                 --build-arg FEDORA="${d#fc}" -t "$(dist_image "$d")" packaging/containers ;;
        debian13) podman build -q -f packaging/containers/client-debian.Containerfile \
                 -t "$(dist_image "$d")" packaging/containers ;;
        *) echo "unknown dist $d" >&2; exit 1 ;;
    esac >/dev/null

    log "client: cargo build for $d"
    # Runtime dependencies: every linked library (as rpm soname provides or
    # the owning deb package) plus what winit/wgpu dlopen at runtime.
    run -e CARGO_TARGET_DIR="/src/target/pkg-$d" -e DIST="$d" "$(dist_image "$d")" bash -euc '
        rustc -V > "/src/target/pkg/rustc-$DIST"
        cargo build --release --locked -p icu-client
        bin=$CARGO_TARGET_DIR/release/icu
        libs=$(ldd "$bin" | awk "/=> \\//{print \$1\" \"\$3}" | grep -v -e ld-linux -e linux-vdso)
        {
            echo "depends:"
            case $DIST in
                fc*)
                    echo "$libs" | awk "{print \"  - \" \$1 \"()(64bit)\"}"
                    printf "  - %s\n" vulkan-loader libxkbcommon libwayland-client ;;
                debian*)
                    echo "$libs" | while read -r _ path; do
                        dpkg -S "$(readlink -f "$path")" 2>/dev/null || dpkg -S "$path"
                    done | cut -d: -f1 | sort -u | sed "s/^/  - /"
                    printf "  - %s\n" libvulkan1 libxkbcommon0 libwayland-client0 ;;
            esac
        } > "/src/target/pkg/depends-$DIST.yaml"'
done

log "supply chain: cargo deny (advisories, bans, licenses, sources) + cargo about"
LICENSES_FILE=$WORK/THIRD-PARTY-LICENSES
podman image exists "$(dist_image debian13)" || podman build -q \
    -f packaging/containers/client-debian.Containerfile -t "$(dist_image debian13)" packaging/containers >/dev/null
run -e CARGO_TARGET_DIR=/src/target/pkg-debian13 "$(dist_image debian13)" bash -euc '
    cargo deny --locked check
    cargo about generate --locked --fail packaging/about.hbs -o target/pkg/THIRD-PARTY-LICENSES'
# Fonts embedded in the client binary are not crates; add their license.
{
    printf '\n%s\n%s\nUsed by:\n  - Inter (crates/icu-client/assets/fonts/inter, embedded)\n\n' \
        '------------------------------------------------------------------------------' \
        'SIL Open Font License 1.1 (OFL-1.1)'
    cat crates/icu-client/assets/fonts/inter/LICENSE.txt
} >> "$LICENSES_FILE"
grep -q 'webrtc-audio-processing' "$LICENSES_FILE" \
    || { echo "THIRD-PARTY-LICENSES lacks webrtc-audio-processing" >&2; exit 1; }

# nfpm expands ${VAR} only in a few fields, not in contents paths, so the
# templates are rendered here. Paths in them are relative to the repo root.
render() {  # render <template> <out> VAR=value...
    local tpl=$1 out=$2; shift 2
    local sed_args=() kv
    for kv in "$@"; do sed_args+=(-e "s|\${${kv%%=*}}|${kv#*=}|g"); done
    sed "${sed_args[@]}" "$tpl" > "$out"
    if grep -n '\${' "$out"; then echo "unrendered variable in $out" >&2; exit 1; fi
}
nfpm() {  # nfpm <config> <format>
    podman run --rm --security-opt label=disable -v "$ROOT":/src -w /src \
        "$NFPM_IMAGE" package -f "$1" -p "$2" -t "$STAGE/"
}

log "nfpm: icu-server"
render packaging/nfpm/icu-server.yaml "$WORK/icu-server.yaml" \
    VERSION="$VERSION" RELEASE="$RELEASE" SERVER_BIN="$SERVER_BIN" LICENSES_FILE="$LICENSES_FILE"
for fmt in rpm deb; do
    nfpm "$WORK/icu-server.yaml" "$fmt"
done

for d in $DISTS; do
    log "nfpm: icu for $d"
    cfg=$WORK/icu-$d.yaml
    render packaging/nfpm/icu.yaml "$cfg" \
        VERSION="$VERSION" RELEASE="$(dist_release "$d")" \
        CLIENT_BIN="target/pkg-$d/release/icu" LICENSES_FILE="$LICENSES_FILE"
    cat "$WORK/depends-$d.yaml" >> "$cfg"
    nfpm "$cfg" "$(dist_format "$d")"
done

mapfile -t packages < <(cd "$STAGE" && ls)
mv -- "$STAGE"/* "$OUT"/
{
    echo "commit $COMMIT"
    if [ "${ALLOW_DIRTY:-0}" = 1 ]; then echo DIRTY; fi
    for f in "$WORK"/rustc-*; do echo "rustc ${f##*/rustc-}: $(cat "$f")"; done
    printf 'artifact %s\n' "${packages[@]}"
} > "$OUT/BUILDINFO.linux"
packaging/checksums.sh "$OUT"

if [ "${SKIP_SMOKE:-0}" != 1 ]; then
    VERSION=$VERSION RELEASE=$RELEASE DISTS=$DISTS SERVER_DISTS=${SERVER_DISTS-"debian12 alma10"} \
        packaging/smoke.sh
fi

log "done: $OUT"
ls -l "$OUT"
