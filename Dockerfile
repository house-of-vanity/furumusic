FROM rust:1-slim AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock* ./
COPY build.rs ./build.rs
COPY prompts ./prompts
COPY src ./src
COPY templates ./templates

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /data
COPY --from=builder /app/target/release/furumusic /usr/local/bin/furumusic

EXPOSE 8000
CMD ["furumusic", "-l", "0.0.0.0:8000"]
