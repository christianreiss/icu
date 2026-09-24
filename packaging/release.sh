#!/usr/bin/env bash
# Mirrors main and the tags to GitHub and publishes dist/<version>/ as the
# release v<version>. Run packaging/build.sh first, on the tagged commit.
#
#   git tag -a v0.1.0 -m "ICU 0.1.0" && packaging/build.sh && packaging/release.sh
#
# Environment:
#   GH_REPO=christianreiss/icu   target repository (created public if missing)
#
# git.alpha-labs.net (origin) stays the primary remote; GitHub is a mirror.
set -euo pipefail

cd "$(dirname "$0")/.."
GH_REPO=${GH_REPO:-christianreiss/icu}
GITLEAKS_IMAGE=docker.io/zricethezav/gitleaks:v8.30.1

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
TAG=v$VERSION
OUT=dist/$VERSION

die() { echo "release: $*" >&2; exit 1; }
log() { printf '\n==> %s\n' "$*"; }

[ -z "$(git status --porcelain)" ] || die "working tree is dirty"
[ "$(git rev-parse --abbrev-ref HEAD)" = main ] || die "not on main"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || die "tag $TAG missing"
[ "$(git rev-parse "$TAG^{commit}")" = "$(git rev-parse HEAD)" ] || die "$TAG is not HEAD"
[ -f "$OUT/SHA256SUMS" ] || die "$OUT/SHA256SUMS missing, run packaging/build.sh"
(cd "$OUT" && sha256sum -c --quiet SHA256SUMS) || die "checksums in $OUT do not match"

log "secret scan of the full history"
# Known false positives (test fixtures) are listed in .gitleaksignore.
podman run --rm --security-opt label=disable -v "$PWD":/repo:ro "$GITLEAKS_IMAGE" \
    git --no-banner --redact --gitleaks-ignore-path /repo/.gitleaksignore /repo \
    || die "gitleaks found something; check before publishing"
if git log -p --all | grep -q 'BEGIN [A-Z ]*PRIVATE KEY'; then
    die "a PEM private key appears in the history"
fi

log "GitHub repository $GH_REPO"
if ! gh repo view "$GH_REPO" >/dev/null 2>&1; then
    gh repo create "$GH_REPO" --public \
        --description "ICU: minimal low-latency voice for a fixed group (QUIC, mTLS, Opus)"
fi
git remote get-url github >/dev/null 2>&1 \
    || git remote add github "https://github.com/$GH_REPO.git"

log "push main and tags"
git push github main --tags

log "release $TAG"
notes=target/pkg/release-notes.md
mkdir -p target/pkg
cat > "$notes" <<EOF
ICU $VERSION

**Server** (static binary, same package for Fedora and Debian):

    sudo dnf install ./icu-server-$VERSION-*.x86_64.rpm     # Fedora
    sudo apt install ./icu-server_${VERSION}-*_amd64.deb     # Debian

then set \`server_name\`/\`public_host\` in \`/etc/icu/server.toml\` and
\`sudo systemctl enable --now icu-server\`. Details: \`deploy/README.md\`.

**Client** (\`icu\`), one package per distro:

    sudo dnf install ./icu-$VERSION-*.fc43.x86_64.rpm       # Fedora 43 (fc42 for Fedora 42)
    sudo apt install ./icu_${VERSION}-*+deb13_amd64.deb      # Debian 13

Verify downloads with \`sha256sum -c SHA256SUMS\`.

The client contains OpenH264 built from source; Cisco's patent license covers
only Cisco's own binaries. Third-party notices: \`/usr/share/licenses/icu*/THIRD-PARTY-LICENSES\`.
EOF

if gh release view "$TAG" --repo "$GH_REPO" >/dev/null 2>&1; then
    gh release upload "$TAG" "$OUT"/* --repo "$GH_REPO" --clobber
else
    gh release create "$TAG" "$OUT"/* --repo "$GH_REPO" --verify-tag \
        --title "ICU $VERSION" --notes-file "$notes"
fi
gh release view "$TAG" --repo "$GH_REPO"
