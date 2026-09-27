#!/usr/bin/env bash
# Mirrors main and the tags to GitHub and publishes dist/<version>/ as the
# release v<version>. Run packaging/build.sh (and packaging/windows/build.sh)
# first, on the tagged commit.
#
#   git tag -s v0.1.0 -m "ICU 0.1.0" && packaging/build.sh \
#       && packaging/windows/build.sh && packaging/release.sh
#
# Published are exactly the BUILDINFO.* files and the artifacts they list
# (packaging/checksums.sh --list), SHA256SUMS and its signature
# SHA256SUMS.sig. The tag must be signed (git tag -s), every BUILDINFO must
# name HEAD and none may say DIRTY. SHA256SUMS is signed with the release
# key (ssh-keygen -Y sign, namespace icu-release) and checked against
# packaging/allowed_signers, the public half users verify with
# (deploy/README.md §0). An asset already in the GitHub release is never
# replaced; remove it deliberately with `gh release delete-asset`.
#
# Environment:
#   GH_REPO=christianreiss/icu          target repository (created public if missing)
#   ICU_RELEASE_KEY=~/.ssh/icu-release  SSH private key that signs SHA256SUMS
#
# git.alpha-labs.net (origin) stays the primary remote; GitHub is a mirror.
set -euo pipefail

cd "$(dirname "$0")/.."
GH_REPO=${GH_REPO:-christianreiss/icu}
GITLEAKS_IMAGE=docker.io/zricethezav/gitleaks:v8.30.1
ICU_RELEASE_KEY=${ICU_RELEASE_KEY:-$HOME/.ssh/icu-release}
ALLOWED_SIGNERS=packaging/allowed_signers

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
TAG=v$VERSION
OUT=dist/$VERSION

die() { echo "release: $*" >&2; exit 1; }
log() { printf '\n==> %s\n' "$*"; }

[ -z "$(git status --porcelain)" ] || die "working tree is dirty"
[ "$(git rev-parse --abbrev-ref HEAD)" = main ] || die "not on main"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || die "tag $TAG missing"
[ "$(git rev-parse "$TAG^{commit}")" = "$(git rev-parse HEAD)" ] || die "$TAG is not HEAD"
git tag -v "$TAG" >/dev/null 2>&1 \
    || die "tag $TAG is not signed or its signature does not verify (git tag -s; see git tag -v $TAG)"

# Provenance: every build in $OUT is from this commit and a clean tree.
head=$(git rev-parse HEAD)
compgen -G "$OUT/BUILDINFO.*" >/dev/null || die "$OUT has no BUILDINFO, run packaging/build.sh"
for info in "$OUT"/BUILDINFO.*; do
    [ "$(sed -n 's/^commit //p' "$info")" = "$head" ] \
        || die "${info##*/} is from another commit; rebuild it on $TAG (or remove it and its artifacts)"
    if grep -qx DIRTY "$info"; then
        die "${info##*/} was built from a dirty tree (ALLOW_DIRTY=1)"
    fi
done
[ -f "$OUT/SHA256SUMS" ] || die "$OUT/SHA256SUMS missing, run packaging/build.sh"
mapfile -t files < <(packaging/checksums.sh --list "$OUT")
[ "$(awk '{print $2}' "$OUT/SHA256SUMS" | LC_ALL=C sort)" = "$(printf '%s\n' "${files[@]}")" ] \
    || die "SHA256SUMS does not cover exactly the BUILDINFO files and their artifacts (packaging/checksums.sh $OUT)"
(cd "$OUT" && sha256sum -c --quiet SHA256SUMS) || die "checksums in $OUT do not match"

# The release key is the owner's; this script never creates it.
publish_key="printf 'icu-release namespaces=\"icu-release\" %s\\n' \"\$(cut -d' ' -f1,2 $ICU_RELEASE_KEY.pub)\" > $ALLOWED_SIGNERS"
[ -f "$ICU_RELEASE_KEY" ] || die "signing key $ICU_RELEASE_KEY missing (ICU_RELEASE_KEY). Create it once
    ssh-keygen -t ed25519 -C icu-release -f $ICU_RELEASE_KEY
and commit its public half, which users verify against:
    $publish_key"
[ -f "$ALLOWED_SIGNERS" ] || die "$ALLOWED_SIGNERS missing; commit the public half of $ICU_RELEASE_KEY:
    $publish_key"

log "secret scan of the full history"
# The default rules plus ICU passports (.gitleaks.toml). Known false
# positives (test fixtures) are listed in .gitleaksignore.
podman run --rm --security-opt label=disable -v "$PWD":/repo:ro "$GITLEAKS_IMAGE" \
    git --no-banner --redact --config /repo/.gitleaks.toml \
    --gitleaks-ignore-path /repo/.gitleaksignore /repo \
    || die "gitleaks found something; check before publishing"
if git log -p --all | grep -q 'BEGIN [A-Z ]*PRIVATE KEY'; then
    die "a PEM private key appears in the history"
fi
# A passport or registration bundle carries a private key too. The armor
# line alone is in docs and tests; a base64 body right after it is not.
if git log -p --all | grep -A1 -e '-----BEGIN ICU PASSPORT-----' \
        | grep -qE '[A-Za-z0-9+/=]{40,}'; then
    die "an ICU passport (private key) appears in the history"
fi

log "sign SHA256SUMS"
rm -f "$OUT/SHA256SUMS.sig"
ssh-keygen -Y sign -f "$ICU_RELEASE_KEY" -n icu-release "$OUT/SHA256SUMS" \
    || die "signing SHA256SUMS with $ICU_RELEASE_KEY failed"
ssh-keygen -Y verify -f "$ALLOWED_SIGNERS" -I icu-release -n icu-release \
    -s "$OUT/SHA256SUMS.sig" < "$OUT/SHA256SUMS" \
    || die "SHA256SUMS.sig does not verify against $ALLOWED_SIGNERS: is $ICU_RELEASE_KEY the release key?"
assets=()
for f in "${files[@]}" SHA256SUMS SHA256SUMS.sig; do assets+=("$OUT/$f"); done

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

**Server** (static binary, one package for Fedora, AlmaLinux 10+ and Debian 12+):

    sudo dnf install ./icu-server-$VERSION-*.x86_64.rpm     # Fedora, AlmaLinux/EL 10+
    sudo apt install ./icu-server_${VERSION}-*_amd64.deb     # Debian 12+

then check \`server_name\`/\`public_host\` in \`/etc/icu/server.toml\` (unset: the
host's FQDN, if it resolves to this host) and \`sudo systemctl enable --now icu-server\`. Details: \`deploy/README.md\`.

**Client** (\`icu\`), one package per distro:

    sudo dnf install ./icu-$VERSION-*.fc43.x86_64.rpm       # Fedora 43 (fc42 for Fedora 42)
    sudo apt install ./icu_${VERSION}-*+deb13_amd64.deb      # Debian 13

**Client for Windows 10/11** (portable, unsigned): unzip
\`icu-$VERSION-windows-x86_64.zip\` and start \`icu.exe\`. SmartScreen warns about
an unknown publisher on the first start: "More info" → "Run anyway". Details:
\`README.txt\` in the ZIP.

**Verify** before installing: \`SHA256SUMS.sig\` is an SSH signature of
\`SHA256SUMS\` by the ICU release key. Take \`allowed_signers\` from
\`packaging/allowed_signers\` in the repository once and keep it (compare its
key with the owner on another channel); a copy fetched next to the downloads
proves nothing. Then:

    ssh-keygen -Y verify -f allowed_signers -I icu-release -n icu-release -s SHA256SUMS.sig < SHA256SUMS
    sha256sum -c --ignore-missing SHA256SUMS

\`BUILDINFO.*\` names the commit and compiler of each build (\`git tag -v $TAG\`).

The client contains OpenH264 built from source; Cisco's patent license covers
only Cisco's own binaries. Third-party notices: \`/usr/share/licenses/icu*/THIRD-PARTY-LICENSES\`.
EOF

if gh release view "$TAG" --repo "$GH_REPO" >/dev/null 2>&1; then
    # Fails on an asset that is already there: a published file is never
    # replaced silently.
    gh release upload "$TAG" "${assets[@]}" --repo "$GH_REPO"
else
    gh release create "$TAG" "${assets[@]}" --repo "$GH_REPO" --verify-tag \
        --title "ICU $VERSION" --notes-file "$notes"
fi
gh release view "$TAG" --repo "$GH_REPO"
