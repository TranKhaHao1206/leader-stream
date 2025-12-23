# syntax=docker/dockerfile:1

FROM rust:1.83-slim AS builder
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        build-essential \
        autoconf \
        automake \
        libtool \
        cmake \
        clang \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown
RUN cargo install wasm-bindgen-cli

COPY leader-stream/Cargo.toml leader-stream/Cargo.lock ./leader-stream/
COPY leader-stream/src ./leader-stream/src
COPY leader-stream/public ./leader-stream/public

WORKDIR /app/leader-stream
RUN cargo build --release
RUN cargo build --release --target wasm32-unknown-unknown --lib
RUN wasm-bindgen \
    --target no-modules \
    --out-name app \
    --out-dir /app/leader-stream/public \
    /app/leader-stream/target/wasm32-unknown-unknown/release/leader_stream.wasm
RUN printf '\nwasm_bindgen("/app_bg.wasm");\n' >> /app/leader-stream/public/app.js
RUN rm -f /app/leader-stream/public/app.d.ts /app/leader-stream/public/app_bg.wasm.d.ts

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r app && useradd -r -g app app

COPY --from=builder /app/leader-stream/target/release/leader-stream /usr/local/bin/leader-stream
COPY --from=builder /app/leader-stream/public /app/public

ENV PORT=3000
ENV STATIC_DIR=/app/public
EXPOSE 3000

USER app
CMD ["/usr/local/bin/leader-stream"]
