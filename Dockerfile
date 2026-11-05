# syntax=docker/dockerfile:1
# Image linux/arm64 (Raspberry Pi 3 B+) construite depuis n'importe quel hote :
#   docker buildx build --platform linux/arm64 -t email-zero:local --load .
# Le stage builder tourne sur $BUILDPLATFORM (natif) et cross-compile le
# binaire ; seul le stage final est arm64.

FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       binutils-aarch64-linux-gnu \
       gcc-aarch64-linux-gnu \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
RUN rustup target add aarch64-unknown-linux-gnu
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --target aarch64-unknown-linux-gnu \
    && aarch64-linux-gnu-strip target/aarch64-unknown-linux-gnu/release/email-zero

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin emailzero
WORKDIR /app
RUN mkdir -p /app/data && chown emailzero:emailzero /app/data
COPY --from=builder /build/target/aarch64-unknown-linux-gnu/release/email-zero /usr/local/bin/email-zero
COPY prompts ./prompts
USER emailzero
VOLUME ["/app/data"]
ENTRYPOINT ["/usr/local/bin/email-zero"]
CMD ["--config", "/config/config.toml"]
