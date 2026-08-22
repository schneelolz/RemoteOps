FROM rust:1.95-bookworm AS builder

WORKDIR /workspace

COPY Cargo.toml Cargo.lock ./
COPY build ./build
COPY crates ./crates
COPY apps ./apps
COPY tests ./tests

RUN cargo build --release --locked -p remoteops-relay

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /nonexistent --shell /usr/sbin/nologin remoteops \
    && install -d -o remoteops -g remoteops -m 0700 /data /data/tls

COPY --from=builder /workspace/target/release/remoteops-relay /usr/local/bin/remoteops-relay

USER remoteops

VOLUME ["/data"]
EXPOSE 7443 8080

ENV REMOTEOPS_RELAY_BIND=0.0.0.0:7443 \
    REMOTEOPS_HEALTH_BIND=0.0.0.0:8080 \
    REMOTEOPS_TLS_CERT=/data/tls/cert.pem \
    REMOTEOPS_TLS_KEY=/data/tls/key.pem \
    REMOTEOPS_STATE_FILE=/data/relay-state.json \
    REMOTEOPS_LEASE_MINUTES=10 \
    REMOTEOPS_HEARTBEAT_SECONDS=15 \
    RUST_LOG=remoteops_relay=info

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3 \
    CMD curl --fail --silent --show-error http://127.0.0.1:8080/health || exit 1

ENTRYPOINT ["/usr/local/bin/remoteops-relay"]

