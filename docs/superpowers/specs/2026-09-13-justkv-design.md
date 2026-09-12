# justkv — Read-Only In-Memory Key-Value Server

**Date:** 2026-09-13
**Status:** Approved design, pending implementation plan

## 1. Purpose

A read-only key-value server. It loads a dataset from a file into memory at
startup and serves lookups over HTTP. The data never changes at runtime.

That single constraint — read-only — shapes everything below. An immutable map
needs no locks, no sharding, and no synchronization on the read path. Most
key-value store complexity exists to serve writes, and none of it is needed here.

## 2. Requirements

- Load key-value data from a file into memory at startup.
- Expose a `get` API over HTTP/1.1.
- Read-only. No writes, no eviction, no expiry.
- Optimize for throughput, latency, and memory efficiency.
- Provide a tool to package the executable together with its data file.

### Scale

Under ~1M keys and well under 100 MB total. This is small enough that the map
lookup (~20–50 ns) is under 1% of the cost of an HTTP request/response cycle
(~2–20 µs). Performance work therefore targets the HTTP layer, allocation
behavior, and copy avoidance — not the data structure's asymptotics.

### Non-goals

Writes, deletes, TTL, replication, clustering, persistence, authentication,
TLS termination, and batch/multi-key lookup. TLS and authentication belong to
a reverse proxy. Batch get was considered and declined.

## 3. Resolved decisions

| Decision | Choice | Rationale |
|---|---|---|
| Protocol | HTTP/1.1 REST | Universally consumable; adequate throughput with keep-alive |
| Source format | CSV / TSV | Streams cleanly, integrates with existing pipelines |
| Runtime formats | Both CSV and compiled binary | Dev iterates on CSV; prod ships compiled |
| Storage layout | Single arena + 16-byte offset entries | ~16 MB/1M keys vs ~50–60 MB for per-key allocations |
| Validation | Build time, via `justkv check` | A bad dataset fails the build, not a production pod |
| Missing keys | 404, or configurable default value | Default makes the API total for clients that want it |
| Content type | `text/plain; charset=utf-8` | Values are predominantly strings |
| Packaging | Container image | Matches the deployment target |
| Endpoints | get, health, metrics, stats | Batch get explicitly excluded |

## 4. Architecture

```
src/
  main.rs              CLI dispatch: serve | check | build | healthcheck
  config.rs            clap args + environment fallback
  store/
    mod.rs             Store, Entry, from_parts(), get(), stats()
    csv_loader.rs      CSV/TSV -> (arena, entries)
    compiled.rs        binary format reader + writer
  server/
    mod.rs             axum router, listener, graceful shutdown
    handlers.rs        route handlers
  metrics.rs           atomic counters + histogram + text rendering
```

Both loaders produce `(arena, entries)` and converge on a single
`Store::from_parts()`. Everything downstream — table construction, lookup,
stats — is shared. The dual-format cost is two functions, not two subsystems.

## 5. Data model

```rust
struct Entry { k_off: u32, k_len: u32, v_off: u32, v_len: u32 }  // 16 bytes

struct Store {
    arena: Bytes,               // all keys and values, contiguous
    table: HashTable<Entry>,    // hashbrown raw table, foldhash hasher
}
```

`arena` is a `Bytes`, so `get` returns `arena.slice(v_off..v_off + v_len)` —
a refcount increment, not a copy. Serving a 100 KB value costs the same as a
10-byte one.

`foldhash` replaces SipHash. The usual hash-flooding argument does not apply:
the map is immutable, so an attacker cannot insert colliding keys.

`u32` offsets cap the dataset at 4 GB — 40x the expected ceiling. Exceeding it
is a clear error at build time, not a silent wrap.

### CSV/TSV load path

Parse with the `csv` crate as `ByteRecord`, append unescaped key and value
bytes to the arena, record offsets, insert into the table. The arena is
allocated once with `with_capacity(file_len)` and never reallocates.

Slices cannot point into the raw file buffer: RFC 4180 quoting means in-file
bytes are not always the logical value (`""` decodes to `"`). Unescaped bytes
are written into a fresh arena, costing a transient ~2x memory during load,
after which the file buffer is dropped.

- Delimiter inferred from extension: `.tsv` -> tab, otherwise comma. `--delimiter` overrides.
- Header row assumed absent. `--header` skips the first line.
- Exactly two columns required.

### Compiled binary format

All integers little-endian, written and read explicitly — never reinterpreted
from the raw buffer, which avoids both alignment and endianness hazards.

```
offset  size          field
0       8             magic = "JUSTKV01"
8       4             flags (bit 0: values are binary, not UTF-8 validated)
12      4             entry_count
16      4             arena_len
20      4             reserved (zero)
24      16*count      entries, 4 x u32 LE each
...     arena_len     arena bytes
```

Arena offset is `24 + 16 * entry_count`, known immediately after the header.
The reader reads the header, then entries, then the arena directly into an
exact-capacity buffer — no whole-file copy.

Format detection: first 8 bytes match the magic -> compiled; otherwise CSV.

`flags` bit 0 is set by `build` when `--allow-binary` was used. On load it
changes the default content type to `application/octet-stream`, so a binary
dataset stays correctly typed without re-passing the flag at serve time. An
explicit `--content-type` always wins.

## 6. HTTP API

| Route | Behavior |
|---|---|
| `GET /kv/{key}` | Percent-decoded path segment. Primary form. |
| `GET /kv?k={key}` | Escape hatch for keys containing `/` or awkward bytes. |
| `GET /health` | `200 "ok"`. Only reachable after load completes. |
| `GET /metrics` | Prometheus text format. |
| `GET /stats` | JSON: key count, arena bytes, source path and format, load duration, uptime. |

An empty key is legal if present in the dataset, and is reachable only via
`GET /kv?k=` — the path form `GET /kv/` does not match the route and returns
`404` without a lookup.

Both `/kv` forms exist because `%2F` in a path is unreliable: many proxies
decode it before routing, turning `/kv/a%2Fb` into a nested path request. The
query form sidesteps this.

### Response contract

- **Hit:** `200`, `Content-Type: text/plain; charset=utf-8` (overridable via
  `--content-type`), `Content-Length`, body sliced from the arena.
- **Miss, no default:** `404`, empty body.
- **Miss, default configured:** `200`, default bytes, plus `X-JustKV-Default: 1`.

A configured default makes `GET` total — every key returns 200. Because that
removes the miss signal from the status code, a default-served response is
counted as a **miss** in metrics and marked by the header. The status code may
be deliberately forgiving; the observability must not be.

### Serving

- `axum` on a multi-threaded `tokio` runtime, workers = core count (`--workers`).
- `Arc<Store>` in router state; one atomic increment per request.
- Data loads **before** the listener binds, so orchestrators see
  connection-refused during startup rather than 503s.
- `TCP_NODELAY` set explicitly on accepted sockets.
- HTTP keep-alive enabled; no per-request heap allocation on the hot path.
- Graceful shutdown on `SIGTERM`/`SIGINT`, so rolling deploys do not wait out
  the container grace period before `SIGKILL`.

## 7. Metrics

Hand-rolled atomics rather than the `prometheus` crate: six counters and one
fixed-bucket histogram render in ~40 lines and drop a dependency tree.

```
justkv_requests_total
justkv_hits_total
justkv_misses_total
justkv_defaults_served_total
justkv_response_bytes_total
justkv_request_duration_seconds{bucket,sum,count}
justkv_keys                      (gauge)
justkv_arena_bytes               (gauge)
justkv_uptime_seconds            (gauge)
```

All updates use `Ordering::Relaxed`. Histogram buckets: 25 µs, 50 µs, 100 µs,
250 µs, 500 µs, 1 ms, 2.5 ms, 5 ms, 10 ms, 25 ms, 50 ms, 100 ms, +Inf.

Known cost, stated plainly: one `Instant::now()` pair per request (~40 ns,
genuinely larger than the map lookup) and cross-core cache-line contention on
counters under heavy load. `--no-metrics` disables timing for clean benchmarks.

## 8. Configuration

| Flag | Env | Default |
|---|---|---|
| `--data` | `JUSTKV_DATA` | required for `serve` only |
| `--bind` | `JUSTKV_BIND` | `0.0.0.0:8080` |
| `--default-value` | `JUSTKV_DEFAULT` | unset (404 on miss) |
| `--content-type` | `JUSTKV_CONTENT_TYPE` | `text/plain; charset=utf-8` |
| `--delimiter` | — | inferred from extension |
| `--header` | — | false |
| `--allow-binary` | — | false |
| `--workers` | — | core count |
| `--no-metrics` | — | false |

### Subcommands

`check` and `build` take the dataset as a positional argument and ignore
`JUSTKV_DATA`; only `serve` reads it. Parsing flags (`--delimiter`,
`--header`, `--allow-binary`) apply to all three.

- `serve` — load and serve.
- `check <data>` — validate and exit. Reports **every** problem with line
  numbers, not just the first. Exit 0 clean, 1 validation errors, 2 I/O error.
- `build <data> -o <out>` — compile CSV to the binary format.
- `healthcheck` — issue `GET /health` against `JUSTKV_BIND` (rewriting a
  `0.0.0.0` host to `127.0.0.1`), exit 0 on `200` and 1 otherwise. Used as the
  container `HEALTHCHECK` because a `scratch` image has no shell or curl.

### Validation performed by `check`

Duplicate keys (reporting the key and both line numbers), wrong column count,
unparseable rows, values that are not valid UTF-8 (unless `--allow-binary`,
which also flips the default content type to `application/octet-stream`), and
datasets exceeding the 4 GB `u32` limit.

## 9. Packaging

### Dockerfile — three stages

1. **Builder** — `rust:alpine`, target `x86_64-unknown-linux-musl`, fully
   static binary. Release profile: `lto = "fat"`, `codegen-units = 1`,
   `panic = "abort"`, `strip = true`. Expect 3–5 MB.
2. **Data** — runs `justkv check $DATA`, then `justkv build $DATA -o /out/kv.bin`.
   **This stage enforces the build-time-errors requirement**: duplicates,
   malformed rows, or bad UTF-8 fail `docker build`.
3. **Runtime** — `FROM scratch` with only the binary and `/data/kv.bin`.
   `ENV JUSTKV_DATA=/data/kv.bin`, `EXPOSE 8080`, `USER 65534:65534` (numeric,
   since scratch has no `/etc/passwd`), `HEALTHCHECK` via the self-probe.

### scripts/pack.sh

```
./scripts/pack.sh --data data/kv.tsv --tag justkv:v1 [--platform linux/amd64]
```

Validates the dataset locally first — seconds of feedback instead of a full
image build — then invokes `docker buildx build` with the data path as a
build-arg. Platform defaults to `linux/amd64` with an override, since
development is on arm64 macOS while deployment is likely x86; a mismatch
otherwise surfaces as `exec format error` at deploy time.

Because both formats load at runtime, `JUSTKV_DATA` can point at a
volume-mounted CSV in development while production uses the baked binary.

## 10. Testing

**Unit — parser and store.** Delimiter inference; RFC 4180 quoting including
`""` escapes and embedded newlines; empty values; `--header` skipping. Error
cases assert on message content, not merely on failure: duplicates must name
the key and both line numbers. Guardrails: invalid UTF-8 rejected unless
`--allow-binary`; the 4 GB limit producing a clear message.

**Property — format equivalence.** `proptest` generates arbitrary key/value
sets including awkward bytes and quoting, round-trips them through both
loaders, and asserts identical arena contents and identical `get` results for
every present key plus a sample of absent ones. This converts "two load paths
to maintain" into one invariant to hold.

**Integration — real server.** Boot on port `0`, then exercise: hit;
miss -> 404; miss -> default, asserting `X-JustKV-Default`; a key containing
`/` via both the `%2F` path form and the `?k=` query form; `/health`;
`/stats`; `/metrics`. Explicitly assert that a default-served response
increments **miss**, not hit — this guards the silent-failure risk introduced
by defaults.

**Benchmark — the requirements check.** Throughput and memory efficiency are
stated requirements, so they get measured rather than asserted. A generator
produces a 1M-key dataset; `oha` (or `wrk`) drives load against the container.
Recorded: sustained RPS with keep-alive, p99 latency, RSS after load, and
startup time for both formats. Committed as `bench/README.md` with a
reproduction command.

## 11. Rejected alternatives

- **Redis RESP, gRPC, custom binary protocol** — HTTP is universally
  consumable and fast enough at this scale.
- **Thread-per-core with `SO_REUSEPORT`** — optimizes accept-path contention
  this workload will not reach.
- **`mmap` of the data file** — solves startup cost for multi-GB datasets that
  do not exist here.
- **Perfect hashing (`phf`, FST)** — removes ~20 ns from a path already under
  1% of request time.
- **Compile-time embedding via `include_bytes!`** — every data change would
  require a full rebuild and a Rust toolchain.
- **Appending data to the binary with a magic trailer** — unnecessary once the
  artifact is a container image.
- **`HashMap<Box<[u8]>, Bytes>`** — ~50–60 MB of per-entry overhead at 1M keys
  versus ~16 MB for the arena layout; material against a 100 MB dataset.

## 12. Known limits

- Dataset capped at 4 GB by `u32` offsets.
- Transient ~2x memory during CSV load (file buffer plus arena).
- Metrics timing adds ~40 ns per request and contends across cores under load.
- No TLS or authentication; front with a reverse proxy if required.
