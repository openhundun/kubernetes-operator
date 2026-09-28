FROM --platform=${BUILDPLATFORM} docker.io/rust:alpine AS base
ARG TARGETARCH
ENV RUSTUP_DIST_SERVER="https://mirrors.ustc.edu.cn/rust-static"
ENV RUSTUP_UPDATE_ROOT="https://mirrors.ustc.edu.cn/rust-static/rustup"
RUN tee /usr/local/cargo/config.toml > /dev/null <<EOF
[net]
git-fetch-with-cli = true
[registries]
ustc = { index = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/" }
[source]
crates-io = { replace-with = "ustc" }
ustc      = { registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/" }
EOF
RUN \
    TARGET="$(case "${TARGETARCH}" in amd64) echo "x86_64-unknown-linux-musl";; arm64) echo "aarch64-unknown-linux-musl";; *) exit 1;; esac)" && \
    echo "${TARGET}" > /target-triple && \
    sed -i 's|dl-cdn.alpinelinux.org|mirrors.ustc.edu.cn|g' /etc/apk/repositories && \
    apk add --no-cache zig && \
    cargo install cargo-zigbuild && \
    rustup target add "${TARGET}"

FROM --platform=${BUILDPLATFORM} base AS builder
WORKDIR /app
COPY Cargo.toml Cargo.toml
COPY source/flector/Cargo.toml source/flector/Cargo.toml
RUN \
    TARGET="$(cat /target-triple)" && \
    mkdir -p source/flector/src && \
    echo "fn main() {}" > source/flector/src/main.rs && \
    cargo zigbuild --target "${TARGET}" --release --package flector
COPY source/flector/src source/flector/src
RUN \
    TARGET="$(cat /target-triple)" && \
    find source/flector/src -type f -exec touch {} + && \
    cargo zigbuild --target "${TARGET}" --release --package flector

FROM ghcr.io/openhundun/alpine:3 AS runtime
LABEL org.opencontainers.image.source="https://github.com/openhundun/kubernetes-operator"
WORKDIR /app
COPY --from=builder /app/target/*/release/flector /app/flector
ENTRYPOINT ["/app/flector"]
