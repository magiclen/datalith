FROM rust AS builder

RUN apt update && apt install -y libmagic-dev

WORKDIR /build

COPY . .

RUN cargo build --release --no-default-features --features magic


FROM debian:bookworm-slim

RUN adduser --disabled-password \
    --gecos "" \
    --no-create-home \
    user

WORKDIR /app

RUN chown user:user /app

RUN apt update && apt install -y libmagic1

RUN rm -rf /var/lib/apt/lists/*

USER user

COPY --chown=user:user --from=builder /build/target/release/datalith  /app/

ENTRYPOINT ["/app/datalith"]