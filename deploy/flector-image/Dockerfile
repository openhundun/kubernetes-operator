ARG IMAGE_REGISTRY="ghcr.io"

FROM --platform=${BUILDPLATFORM} ${IMAGE_REGISTRY}/openhundun/alpine-llvm-zig-rust:3-21-0.16-nightly AS builder
ARG TARGETARCH
RUN rm -rf /usr/local/cargo/config.toml
RUN \
    TARGET="$(case "${TARGETARCH}" in amd64) echo "x86_64-unknown-linux-musl";; arm64) echo "aarch64-unknown-linux-musl";; *) exit 1;; esac)" && \
    echo "${TARGET}" > /target-triple
WORKDIR /app
COPY Cargo.toml Cargo.toml
COPY source/flector/Cargo.toml source/flector/Cargo.toml
RUN \
    TARGET="$(cat /target-triple)" && \
    for x in source/*/Cargo.toml; do mkdir -p "${x%/*}/src" && touch "${x%/*}/src/lib.rs"; done && \
    cargo zigbuild --release --target "${TARGET}" --package flector
COPY source/flector/src source/flector/src
RUN \
    TARGET="$(cat /target-triple)" && \
    for x in source/flector/src/*; do touch "${x}"; done && \
    cargo zigbuild --release --target "${TARGET}" --package flector

FROM ${IMAGE_REGISTRY}/openhundun/alpine:3 AS runtime
LABEL org.opencontainers.image.source="https://github.com/openhundun/kubernetes-kit"
WORKDIR /app
COPY --from=builder /app/target/*/release/flector /app/flector
ENTRYPOINT ["/app/flector"]
