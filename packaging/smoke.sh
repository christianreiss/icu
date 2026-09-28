#!/usr/bin/env bash
# Installs the packages from dist/<version>/ into clean distro containers and
# checks them: binaries run, group and config ownership, no missing shared
# libraries, the unit verifies, a modified config survives a reinstall, and
# removal keeps the server state. Downloaded packages are cached in podman
# volumes (icu-smoke-*), so only the first run per distro is slow. Called by packaging/build.sh; runnable alone:
#
#   packaging/smoke.sh                         # all distros, version from Cargo.toml
#   DISTS=debian13 packaging/smoke.sh
#   DISTS= SERVER_DISTS="debian12 alma10" packaging/smoke.sh   # server only
#
# DISTS get server and client; SERVER_DISTS only the server (the server
# package is one static binary for every distro, the client is per distro).
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION=${VERSION:-$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)}
RELEASE=${RELEASE:-1}
DISTS=${DISTS-"fc42 fc43 debian13"}
SERVER_DISTS=${SERVER_DISTS-"debian12 alma10"}
OUT=dist/$VERSION

# Shared by both families; $PREP, $INSTALL, $REINSTALL, $REMOVE are set per distro.
# An empty $CLIENT_PKG skips the client.
CHECKS='
set -eux
eval "$PREP"
$INSTALL /pkgs/$SERVER_PKG ${CLIENT_PKG:+/pkgs/$CLIENT_PKG}
icu-server --version
icu-cli --version
getent group icu
getent group icu-monitor
test "$(stat -c %G /etc/icu/server.toml)" = icu
test "$(stat -c %a /etc/icu/server.toml)" = 640
systemd-analyze verify /usr/lib/systemd/system/icu-server.service
test -f /usr/lib/sysusers.d/icu-server.conf
test -f /usr/share/licenses/icu-server/THIRD-PARTY-LICENSES
if [ -n "$CLIENT_PKG" ]; then
    icu --version
    test "$(ldd /usr/bin/icu | grep -c "not found")" -eq 0
    test -f /usr/share/applications/icu.desktop
    grep -qx "MimeType=application/x-icu;" /usr/share/applications/icu.desktop
    test -f /usr/share/mime/packages/icu.xml
    if [ -f /usr/share/mime/globs2 ]; then grep -q ":application/x-icu:\*\.icu$" /usr/share/mime/globs2; fi
    test -f /usr/share/licenses/icu/THIRD-PARTY-LICENSES
fi
echo "# local edit" >> /etc/icu/server.toml
$REINSTALL /pkgs/$SERVER_PKG
grep -q "# local edit" /etc/icu/server.toml
mkdir -p /var/lib/private/icu && touch /var/lib/private/icu/ca.key
$REMOVE icu-server ${CLIENT_PKG:+icu}
test -f /var/lib/private/icu/ca.key
test ! -e /usr/bin/icu-server && test ! -e /usr/bin/icu-cli && test ! -L /usr/bin/icu-cli && test ! -e /usr/bin/icu
'

smoke() {  # smoke <dist> <with_client: 0|1>
    local d=$1 image server client cache env dnf
    printf '\n==> smoke: %s%s\n' "$d" "$([ "$2" = 1 ] || echo ' (server only)')"
    dnf="dnf -y -q --setopt=keepcache=True --setopt=install_weak_deps=False"
    case $d in
        fc*|alma*)
            case $d in
                fc*) image=registry.fedoraproject.org/fedora:${d#fc}
                     cache=(-v "icu-smoke-dnf-${d}":/var/cache/libdnf5) ;;
                # EL10 still has dnf4 with its cache in /var/cache/dnf.
                alma*) image=docker.io/library/almalinux:${d#alma}
                     cache=(-v "icu-smoke-dnf-${d}":/var/cache/dnf) ;;
            esac
            server=icu-server-$VERSION-$RELEASE.x86_64.rpm
            client=icu-$VERSION-$RELEASE.$d.x86_64.rpm
            env=(-e PREP=: -e INSTALL="$dnf install" -e REINSTALL="$dnf reinstall"
                 -e REMOVE="$dnf remove")
            ;;
        debian*)
            image=docker.io/library/debian:${d#debian}
            server=icu-server_${VERSION}-${RELEASE}_amd64.deb
            client=icu_${VERSION}-${RELEASE}+deb${d#debian}_amd64.deb
            # The image deletes downloaded debs after install; keep them.
            cache=(-v "icu-smoke-apt-${d}":/var/cache/apt/archives)
            env=(-e DEBIAN_FRONTEND=noninteractive
                 -e PREP="rm -f /etc/apt/apt.conf.d/docker-clean && apt-get -qq update"
                 -e INSTALL="apt-get -y -qq --no-install-recommends install"
                 -e REINSTALL="apt-get -y -qq install --reinstall"
                 -e REMOVE="apt-get -y -qq remove")
            ;;
        *) echo "unknown dist $d" >&2; exit 1 ;;
    esac
    [ "$2" = 1 ] || client=
    for f in "$server" $client; do
        [ -f "$OUT/$f" ] || { echo "missing $OUT/$f" >&2; exit 1; }
    done
    podman run --rm --security-opt label=disable -v "$PWD/$OUT":/pkgs:ro \
        --label "net.icu.build-run=${ICU_BUILD_RUN_ID:-standalone}" \
        "${cache[@]}" -e SERVER_PKG="$server" -e CLIENT_PKG="$client" "${env[@]}" \
        "$image" bash -c "$CHECKS"
    echo "ok: $d"
}

for d in $DISTS; do smoke "$d" 1; done
for d in $SERVER_DISTS; do smoke "$d" 0; done
