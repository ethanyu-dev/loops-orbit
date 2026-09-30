FROM rust:1.98.1-bookworm AS server
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
COPY apps/server apps/server
RUN cargo build --release --locked -p orbit-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --uid 10001 --create-home orbit
WORKDIR /app
RUN mkdir -p /app/memory && chown orbit:orbit /app/memory
COPY --from=server /build/target/release/orbit-server /usr/local/bin/orbit-server
ENV PORT=8080 MEMORY_DIR=/app/memory
USER orbit
EXPOSE 8080
CMD ["orbit-server"]
