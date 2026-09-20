# ICU server image.
#
# `FROM scratch` — the binary is fully static (musl, rustls/ring, no libc, no
# CA bundle: the server is its own CA), so the image needs nothing else.
#
# Build the binary first, outside this Dockerfile (see deploy/README.md):
#
#   podman run --rm -v "$PWD":/src:Z -w /src docker.io/rust:1-alpine \
#     sh -c 'apk add --no-cache musl-dev && \
#            cargo build --release -p icu-server --target x86_64-unknown-linux-musl'
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

FROM scratch

COPY target/x86_64-unknown-linux-musl/release/icu-server /icu-server

# State, runtime and configuration. Declare the state volume so an accidental
# `podman rm` does not take the CA with it.
VOLUME ["/var/lib/icu"]

EXPOSE 4711/udp

# Numeric, because a scratch image has no /etc/passwd. Give the volume and
# /run/icu to the same uid on the host.
USER 65532:65532

ENTRYPOINT ["/icu-server"]
CMD ["run", "--config", "/etc/icu/server.toml"]
