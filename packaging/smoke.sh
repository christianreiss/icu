#!/usr/bin/env bash
# Installs the packages from dist/<version>/ into clean distro containers and
# checks them: binaries run, group and config ownership, no missing shared
# libraries, the unit verifies, a modified config survives a reinstall, and
# removal keeps the server state. Downloaded packages are cached in podman
# volumes (icu-smoke-*), so only the first run per distro is slow. Called by packaging/build.sh; runnable alone:
#
#   packaging/smoke.sh                         # all distros, version from Cargo.toml
#   DISTS=debian13 packaging/smoke.sh
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION=${VERSION:-$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)}
RELEASE=${RELEASE:-1}
DISTS=${DISTS:-"fc42 fc43 debian13"}
OUT=dist/$VERSION

# Shared by both families; $PREP, $INSTALL, $REINSTALL, $REMOVE are set per distro.
CHECKS='
set -eux
eval "$PREP"
$INSTALL /pkgs/$SERVER_PKG /pkgs/$CLIENT_PKG
icu-server --version
icu --version
getent group icu
test "$(stat -c %G /etc/icu/server.toml)" = icu
test "$(stat -c %a /etc/icu/server.toml)" = 640
test "$(ldd /usr/bin/icu | grep -c "not found")" -eq 0
systemd-analyze verify /usr/lib/systemd/system/icu-server.service
test -f /usr/share/applications/icu.desktop
test -f /usr/share/licenses/icu/THIRD-PARTY-LICENSES
echo "# local edit" >> /etc/icu/server.toml
$REINSTALL /pkgs/$SERVER_PKG
grep -q "# local edit" /etc/icu/server.toml
mkdir -p /var/lib/private/icu && touch /var/lib/private/icu/ca.key
$REMOVE icu-server icu
test -f /var/lib/private/icu/ca.key
test ! -e /usr/bin/icu-server && test ! -e /usr/bin/icu
'

for d in $DISTS; do
    printf '\n==> smoke: %s\n' "$d"
    case $d in
        fc*)
            image=registry.fedoraproject.org/fedora:${d#fc}
            server=icu-server-$VERSION-$RELEASE.x86_64.rpm
            client=icu-$VERSION-$RELEASE.$d.x86_64.rpm
            cache=(-v "icu-smoke-dnf-${d}":/var/cache/libdnf5)
            dnf="dnf -y -q --setopt=keepcache=True --setopt=install_weak_deps=False"
            env=(-e PREP=: -e INSTALL="$dnf install" -e REINSTALL="$dnf reinstall"
                 -e REMOVE="$dnf remove")
            ;;
        debian*)
            image=docker.io/library/debian:trixie
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
    for f in "$server" "$client"; do
        [ -f "$OUT/$f" ] || { echo "missing $OUT/$f" >&2; exit 1; }
    done
    podman run --rm --security-opt label=disable -v "$PWD/$OUT":/pkgs:ro \
        "${cache[@]}" -e SERVER_PKG="$server" -e CLIENT_PKG="$client" "${env[@]}" \
        "$image" bash -c "$CHECKS"
    echo "ok: $d"
done
