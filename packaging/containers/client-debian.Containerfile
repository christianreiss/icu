# Build environment for the ICU client on Debian 13 (trixie).
# Debian's rustc (1.85) is older than rust-version 1.98, so the toolchain comes
# from the official rust image, which is itself trixie-based.
FROM docker.io/library/rust:1.98.1-trixie
# License tooling, pinned (packaging/build.sh runs it in this image). First, so
# that changes to the package list below do not recompile it.
RUN cargo install --locked cargo-deny@0.20.2 \
 && cargo install --locked --features cli cargo-about@0.9.2 \
 && rm -rf /usr/local/cargo/registry
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      clang cmake pkg-config meson ninja-build libabsl-dev libpipewire-0.3-dev \
      libxkbcommon-dev libwayland-dev libegl-dev libx11-dev \
      llvm nasm linux-libc-dev \
 && rm -rf /var/lib/apt/lists/*
# webrtc-audio-processing-sys expects rust-objcopy in the rustc sysroot
# (docs/reference/client.md §12); here it lives inside the container only.
RUN d="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin" \
 && mkdir -p "$d" && ln -sfn "$(command -v llvm-objcopy)" "$d/rust-objcopy"
# Debian's abseil (20240722) builds string_view and throw_delegate as separate
# libraries; webrtc-audio-processing-sys only asks pkg-config for absl_base and
# absl_strings, so link the other two explicitly.
ENV CARGO_HOME=/cargo PATH=/usr/local/cargo/bin:$PATH \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-labsl_string_view -C link-arg=-labsl_throw_delegate"
