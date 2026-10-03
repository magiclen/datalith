FROM rust:1.99.0-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends build-essential pkg-config libmagic-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
RUN cargo build --locked --release -p datalith --no-default-features --features magic

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends libmagic1 ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home datalith \
    && mkdir -p /app/data && chown -R datalith:datalith /app

WORKDIR /app
ENV DATALITH_ENVIRONMENT=/app/data
COPY --from=builder /build/target/release/datalith /usr/local/bin/datalith
USER datalith
EXPOSE 1111
ENTRYPOINT ["/usr/local/bin/datalith"]
