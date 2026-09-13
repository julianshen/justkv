# syntax=docker/dockerfile:1

# --- Stage 1: build a fully static binary -----------------------------------
FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# BuildKit sets TARGETARCH from --platform. This stage runs on that platform,
# so the matching musl target is the native one. Hardcoding x86_64 meant
# `pack.sh --platform linux/arm64` either failed on a missing target or, worse,
# produced an arm64 image holding an x86-64 binary that died with an
# exec-format error at startup.
ARG TARGETARCH
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH:-<unset>}" >&2; exit 1 ;; \
    esac; \
    rustup target add "$target"; \
    cargo build --release --target "$target"; \
    cp "target/$target/release/justkv" /justkv

# --- Stage 2: validate and compile the dataset ------------------------------
# This stage is the enforcement point: a duplicate key, a malformed row, or a
# non-UTF-8 value fails `docker build` rather than crash-looping in production.
FROM builder AS data
ARG DATA=data/example.tsv
# One arg per option. A single flag string would be re-split inside the shell
# here, so a delimiter of `*`, `?` or a space would glob against the container
# filesystem instead of reaching the CLI as one argument.
ARG DELIMITER=""
ARG HEADER=0
ARG ALLOW_BINARY=0
# Copy into a DIRECTORY (note the trailing slash) so the original filename —
# and with it the extension that delimiter inference depends on — survives.
COPY ${DATA} /input/
RUN set -eu; \
    f=$(find /input -type f | head -1); \
    set --; \
    if [ -n "$DELIMITER" ]; then set -- "$@" --delimiter "$DELIMITER"; fi; \
    if [ "$HEADER" = "1" ]; then set -- "$@" --header; fi; \
    if [ "$ALLOW_BINARY" = "1" ]; then set -- "$@" --allow-binary; fi; \
    /justkv check "$f" "$@"; \
    /justkv build "$f" -o /kv.bin "$@"

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
