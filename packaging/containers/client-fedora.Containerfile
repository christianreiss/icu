# Build environment for the ICU client on Fedora. ARG FEDORA selects the
# release. Fedora 43 ships rust 1.98.1; Fedora 42 only 1.95.0, below
# rust-version 1.98, so there the toolchain is copied from the official rust
# image (static rustup layout, runs on any recent glibc).
ARG FEDORA=43
FROM docker.io/library/rust:1.98.1-slim-trixie AS upstream-rust

FROM registry.fedoraproject.org/fedora:${FEDORA}
ARG FEDORA
RUN dnf -y install --setopt=install_weak_deps=False \
      gcc gcc-c++ clang cmake pkgconf-pkg-config meson ninja-build abseil-cpp-devel \
      pipewire-devel libxkbcommon-devel wayland-devel mesa-libEGL-devel libX11-devel \
      llvm nasm kernel-headers \
 && dnf clean all
COPY --from=upstream-rust /usr/local/rustup /usr/local/rustup
COPY --from=upstream-rust /usr/local/cargo/bin /usr/local/cargo/bin
ENV RUSTUP_HOME=/usr/local/rustup
RUN if [ "$FEDORA" -ge 43 ]; then \
      rm -rf /usr/local/rustup /usr/local/cargo && dnf -y install rust cargo && dnf clean all; \
    fi
ENV PATH=/usr/local/cargo/bin:$PATH CARGO_HOME=/cargo
RUN d="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin" \
 && mkdir -p "$d" && ln -sfn "$(command -v llvm-objcopy)" "$d/rust-objcopy" \
 && rustc --version
