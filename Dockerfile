# syntax=docker/dockerfile:1

# --- Stage 1: build a fully static binary -----------------------------------
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --target x86_64-unknown-linux-musl
RUN cp target/x86_64-unknown-linux-musl/release/justkv /justkv

# --- Stage 2: validate and compile the dataset ------------------------------
# This stage is the enforcement point: a duplicate key, a malformed row, or a
# non-UTF-8 value fails `docker build` rather than crash-looping in production.
FROM builder AS data
ARG DATA=data/example.tsv
ARG CHECK_FLAGS=""
# Copy into a DIRECTORY (note the trailing slash) so the original filename —
# and with it the extension that delimiter inference depends on — survives.
COPY ${DATA} /input/
RUN set -eu; f=$(find /input -type f | head -1); /justkv check "$f" ${CHECK_FLAGS}
RUN set -eu; f=$(find /input -type f | head -1); /justkv build "$f" -o /kv.bin ${CHECK_FLAGS}

# --- Stage 3: runtime -------------------------------------------------------
FROM scratch
COPY --from=builder /justkv /justkv
COPY --from=data /kv.bin /data/kv.bin
ENV JUSTKV_DATA=/data/kv.bin
ENV JUSTKV_BIND=0.0.0.0:8080
EXPOSE 8080
# Numeric because scratch has no /etc/passwd to resolve a name against.
USER 65534:65534
HEALTHCHECK --interval=10s --timeout=3s --start-period=2s --retries=3 \
  CMD ["/justkv", "healthcheck"]
ENTRYPOINT ["/justkv"]
CMD ["serve"]
