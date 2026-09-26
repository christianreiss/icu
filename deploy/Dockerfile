# ICU server image.
#
# `FROM scratch` — the binary is fully static (musl, rustls/ring, no libc, no
# CA bundle: the server is its own CA), so the image needs nothing else.
#
# Build the binary first, outside this Dockerfile (see deploy/README.md):
#
#   podman run --rm -v "$PWD":/src:Z -w /src docker.io/rust:1-alpine \
#     sh -c 'apk add --no-cache musl-dev && \
#            cargo build --profile release-server -p icu-server --target x86_64-unknown-linux-musl'
#   podman build -f deploy/Dockerfile -t icu-server .
#
# Then run it with the state and config mounted:
#
#   podman run --rm -p 4711:4711/udp \
#     -v icu-state:/var/lib/icu -v ./server.toml:/etc/icu/server.toml:ro \
#     icu-server
#
# The admin CLI runs inside the same container, because it talks to
# /run/icu/admin.sock:
#
#   podman exec <container> /icu-server invite --name alice

# A scratch image has no /run and no /var/lib. The server runs as 65532 and
# cannot create them under the root-owned /, so without these the admin
# socket (/run/icu/admin.sock) is missing and a fresh named volume on
# /var/lib/icu is not writable: no `invite`, no first admin.
FROM docker.io/library/alpine:3 AS dirs
RUN mkdir -p /out/run/icu /out/var/lib/icu

FROM scratch

COPY --from=dirs --chown=65532:65532 /out/ /
COPY target/x86_64-unknown-linux-musl/release-server/icu-server /icu-server

# State, runtime and configuration. Declare the state volume so an accidental
# `podman rm` does not take the CA with it.
VOLUME ["/var/lib/icu"]

EXPOSE 4711/udp

# Numeric, because a scratch image has no /etc/passwd. A new named volume
# takes the owner of /var/lib/icu from the image; a bind mount must belong to
# this uid (under rootless podman: `-v …:/var/lib/icu:U`).
USER 65532:65532

ENTRYPOINT ["/icu-server"]
CMD ["run", "--config", "/etc/icu/server.toml"]
