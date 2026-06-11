# Multi-stage build: compile with the full toolchain, ship a slim runtime.
FROM rust:1-bookworm AS builder
WORKDIR /build
# aws-lc-sys (str0m's crypto) compiles C and needs cmake
RUN apt-get update && apt-get install -y --no-install-recommends cmake && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN useradd --system --no-create-home sfu
COPY --from=builder /build/target/release/soli-sfu /usr/local/bin/soli-sfu
COPY config.example.toml /etc/soli/sfu.toml
USER sfu
# media (UDP) + control (HTTP). Run with --network host in production so the
# advertised public_ip is genuinely reachable on the media port.
EXPOSE 3478/udp 9300/tcp
ENTRYPOINT ["/usr/local/bin/soli-sfu"]
CMD ["/etc/soli/sfu.toml"]
