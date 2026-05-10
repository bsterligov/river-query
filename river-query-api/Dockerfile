FROM rust:1.95-slim-trixie AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y curl && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY infra/migrations ./infra/migrations
RUN cargo build --release --package river-query-api

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/river-query-api /usr/local/bin/river-query-api
CMD ["river-query-api"]
