# justkv benchmarks

Reproducible measurement of the throughput and memory-efficiency requirements
from the spec. Re-run after any change to the store or HTTP layer.

The numbers below were measured on a remote x86_64 Linux host (AMD Ryzen 7
4800U with Radeon Graphics, 16 logical CPUs), by the controller running this
task — not on this development machine, which does not have the disk
headroom for a 1,000,000-row dataset plus a release build. The server ran
natively (not containerised); the load generator (`ab`) ran co-located on
the same host. Every value cell in the Results table below is either an
observed measurement, a configured input, or explicitly marked
`_pending measurement_` where a figure was not collected — none are
estimated, extrapolated, or guessed. The separate "Memory analysis"
section below the table is explicitly labelled as an estimate and draws
out which of its claims are measured versus inferred.

## Setup

    cargo run --release --example gen -- 1000000 bench/kv.tsv
    cargo run --release -- build bench/kv.tsv -o bench/kv.bin
    cargo run --release -- serve --data bench/kv.bin --bind 127.0.0.1:8200

## Load

The measurements below were produced with ApacheBench (`ab`), keep-alive
enabled:

    ab -k -c 50 -n 200000 http://127.0.0.1:8200/kv/key500000

`oha` is an equally valid alternative load generator for this endpoint if
you don't have `ab` installed:

    oha -z 30s -c 50 --no-tui http://127.0.0.1:8200/kv/key500000

## Results

| Measurement | Value |
|---|---|
| Machine | AMD Ryzen 7 4800U with Radeon Graphics, 16 logical CPUs, Linux |
| Rows | 1,000,000 |
| Source TSV size | 38 MB |
| Compiled size | 52 MB |
| RSS after load | 76,108 kB (74.3 MiB), compiled format |
| RSS overhead vs data | arena_bytes = 37,777,780 (~36.0 MiB); RSS is ~74.3 MiB — see Memory analysis below |
| Startup, CSV | 547.97 ms |
| Startup, compiled | 141.03 ms (~3.9x faster than CSV) |
| Throughput | 43,857.90 req/s (hit), 44,302.68 req/s (miss), 43,921.04 req/s (`--no-metrics`) — see caveat below |
| p50 / p99 latency | 1 ms / 2 ms (mean 1.143 ms, c=50) |
| Image size | 2.92 MB |

### Supporting detail

- `justkv build` compile time: 0.50 s real.
- Memory, compiled format: VmRSS after load 76,108 kB (74.3 MiB); VmRSS
  after serving 250,000 requests 78,884 kB (77.0 MiB); VmHWM (peak)
  108,492 kB (106.0 MiB).
- Memory, CSV format: VmRSS after load 76,424 kB (74.6 MiB); VmHWM (peak)
  113,196 kB (110.5 MiB).
- Throughput run: `ab -k -c 50 -n 200000`. Hit (`/kv/key500000`):
  43,857.90 req/s, 0 failed requests, transfer 7,281 KB/s. Miss
  (`/kv/definitely_absent`): 44,302.68 req/s, 50,000 non-2xx responses
  (correct — all 404). With `--no-metrics`: 43,921.04 req/s, 0 failed.
- `/metrics` after the run, confirming correct accounting:
  `justkv_requests_total 250000`, `justkv_hits_total 200000`,
  `justkv_misses_total 50000`, `justkv_keys 1000000`,
  `justkv_arena_bytes 37777780`.

## Memory analysis (estimate, not measured)

Only total process RSS (and VmHWM) were measured. The breakdown below is a
plausibility check on those numbers, not an observation — nothing here was
instrumented, and no tool broke down the process's memory into arena vs.
hash table vs. everything else.

- Arena: 37,777,780 bytes (~36.0 MiB) — **measured**, reported by `/stats`
  as `arena_bytes`.
- Hash table: **inference**. `Entry` is 16 bytes (four `u32` fields:
  `k_off`, `k_len`, `v_off`, `v_len` — verifiable in `src/store/mod.rs`).
  With 1,000,000 entries, and assuming hashbrown rounds the slot count up
  to a power of two, this would imply roughly 32 MiB of table storage — but
  the table's actual allocation was never measured directly.
- Total: **inference**. Arena + estimated hash table would account for
  ~68 MiB of the measured 74.3 MiB RSS, with the remaining ~6 MiB
  plausibly binary, runtime, and allocator overhead. The arithmetic is
  consistent with the measured RSS, which is weak evidence the layout
  behaves as designed — it is not proof: a different split summing to the
  same total would look identical from outside.
- VmHWM (~106 MiB compiled, ~110.5 MiB CSV) being higher than steady-state
  RSS is also **inference, not measured**: a plausible explanation is a
  transient double-hold during load (the input file buffer still resident
  while the arena is being populated), which is what the design doc
  predicts — but no instrumentation confirmed that mechanism specifically;
  it is offered as the most likely explanation, not a verified cause.

## Caveats

1. **Throughput figures may be client-bound, not server-bound.** `ab` is
   single-threaded and ran on the same machine as the server. The
   near-identical hit / miss / `--no-metrics` results (43.86k / 44.30k /
   43.92k req/s — a ~1% spread) are consistent with hitting a client-side
   ceiling rather than the server's true maximum. Treat the throughput
   numbers above as a **floor** on server capacity, not a measured
   maximum. A follow-up run with a multi-threaded or remote load generator
   is needed to establish the real ceiling.
2. **Per-request metrics overhead is not measurable at this request
   rate.** `--no-metrics` (43,921.04 req/s) was within noise of the
   default run (43,857.90 req/s hit / 44,302.68 req/s miss) — about a 1%
   spread either way. This is an observation that overhead wasn't visible
   at this load, not proof that per-request timing is free; a more
   sensitive measurement (e.g. server-bound throughput, or CPU-time rather
   than wall-clock) would be needed to actually isolate the cost.
3. **Startup times were slower than the design spec predicted.** The spec
   estimated ~30-50 ms for compiled-format startup and ~200-400 ms for
   CSV. Actual measured values were 141.03 ms (compiled) and 547.97 ms
   (CSV) — both notably slower than predicted. This is recorded here as an
   observation; the spec's estimates were optimistic and are not being
   quietly revised to match.

## Kubernetes load test (k6 on k3s)

A second, independent measurement run on 2026-09-13, specifically to attack
caveat 1 above: the `ab` figures could not be trusted as server throughput
because `ab` is single-threaded and shared a host with the server. k6 is
multi-threaded and ran in its own pod with its own CPU quota, so client and
server contend only through the node.

Every number in this section is measured. Where a ceiling was not reached,
the section says so rather than reporting the highest observed value as a
limit.

### Environment

| | |
|---|---|
| Cluster | k3s v1.34.4+k3s1, single node (control-plane + worker) |
| Node | Fedora 41, AMD Ryzen 7 4800U, 16 logical CPUs, 62 GiB RAM |
| Node baseline | ~6% CPU, ~62% memory from unrelated workloads (steady, sampled after teardown) |
| Server image | `alpine:3.20` running a static-PIE musl `justkv` (2.95 MB) via `hostPath` |
| Dataset | 1,000,000 keys, compiled format, arena 37,777,780 bytes |
| Startup | 160.06 ms to load `kv.bin` in-cluster |
| Generator | `grafana/k6`, separate pod, 4 CPU / 6 GiB |
| Key mix | uniform random over the keyspace, 1 miss in every 10 requests |

The server ran with `--workers` matched to its CPU quota. Left at the
default it sizes the tokio pool from the node's 16 cores and thrashes
against a 4-core cgroup quota.

### Results — server at 4 CPU, one generator

| Scenario | Offered | Achieved | med | p95 | p99 | max | Failures | Server CPU |
|---|---|---|---|---|---|---|---|---|
| load (constant arrival) | 10,000/s | 9,983/s | 0.18 ms | 5.07 ms | 14.76 ms | 57.4 ms | 0 | 762m |
| soak (20 min) | 2,000/s | 2,000/s | 0.35 ms | 0.47 ms | 0.60 ms | 40.0 ms | 0 | 270m median |
| stress (ramp to 50k) | →50,000/s | 16,178/s mean | 0.28 ms | 1.17 ms | 2.14 ms | 42.8 ms | 0 | 1,815m peak |
| spike (500 → 60k → 500) | burst 60,000/s | 20,897/s peak 10s | 0.33 ms | 1.24 ms | 2.19 ms | 42.7 ms | 0 | 1,776m peak |

Zero failed requests in every scenario. 12.9 million requests were served
across the full suite without a restart.

### Results — server at 4 CPU, two generators

One k6 pod on 4 cores tops out near 21,000 req/s, which is *the generator's*
limit, not the server's. Running two generators concurrently:

| | |
|---|---|
| Combined mean | 29,453 req/s |
| Combined sustained (best 10 s) | **32,298 req/s** |
| Combined peak (1 s) | 33,820 req/s |
| p99 | 4.80 ms / 4.99 ms (per generator) |
| Failures | 0 |
| Server CPU | 2,414m of a 4,000m quota — **60%** |

The server was still only 60% busy. **The 4-core ceiling was not reached.**
32,298 req/s is a floor, not a maximum.

### Results — server pinned to 1 CPU

Pinning the server to a single core lets a 4-core generator definitively
outrun it, which is the only configuration here where saturation was
actually observed.

| Offered | Achieved (sustained 10 s) | p95 | p99 | Server CPU | Failures |
|---|---|---|---|---|---|
| →16,000/s | 15,549/s | 0.53 ms | 1.80 ms | 761m (76%) | 0 |
| →55,000/s | **21,872/s** | 4.17 ms | 9.22 ms | **944m (94%)** | 0 |

Throughput stayed flat at 21–22k req/s while the offered rate climbed from
20,000 to 55,000. That plateau, at 94% of one core with rising latency and
3,597 `dropped_iterations`, is the saturation point of a single core.

Behaviour at saturation is worth stating plainly: latency degraded (p99
0.53 ms → 9.22 ms) and the generator could not hand off work, but **no
request failed and the process did not restart**. It queues; it does not
collapse.

### Memory

| | |
|---|---|
| Container RSS, steady | 70–82 MiB (arena is ~36.0 MiB) |
| Soak drift over 20 min | 74 → 78 MiB (+4 MiB), min 74, max 81 |
| Requests during soak | 2,399,457 |

No growth trend across 2.4 million requests, which is what an immutable
arena and a per-request path that allocates nothing should produce.

### Caveats

1. **The 4-core ceiling is still unknown.** Two generators reached 32,298
   req/s with the server at 60% CPU. A third generator, or generators on a
   separate node, would be needed to find the actual limit. Only the
   single-core number (21,872 req/s at 94% CPU) is a measured saturation
   point.
2. **Client and server share one node.** This is a single-node k3s cluster,
   so the generator's 4 cores and the server's 4 come out of the same 16.
   Unrelated workloads on the node used ~6% CPU and ~62% memory, so CPU
   contention from them was negligible; memory was not a constraint at the
   ~80 MiB the server used. Cross-node generation would remove the sharing
   entirely.

   An earlier draft of this section claimed a ~50% CPU baseline. That
   reading was taken moments after a native release build and a 1,000,000
   row dataset generation on the same host, and measured that, not the
   steady state. Corrected rather than left standing: it overstated the
   contention and so flattered the results' caveats.
3. **Do not compare these numbers to an earlier revision of this file.** An
   initial pass used a heavier k6 configuration (response bodies retained,
   two redundant custom `Trend` metrics) and reported lower throughput on 4
   cores than the final harness reports on 1. Every figure in this section
   comes from the same harness; the earlier ones were discarded rather than
   reconciled.
4. **k6 harness notes, recorded because both bit during this run.** Tagging
   requests by URL gives one metric series per key — 400,340 series in five
   seconds against this keyspace, and the generator OOMs before the server
   notices. A constant `name` tag fixes it. Separately, `Trend` metrics
   retain every sample for end-of-test percentiles, so memory scales with
   total iterations: a 20-minute soak at 14k req/s killed a 1 GiB generator
   at 29%. `http_req_failed` also counts 404 as failure, which reports this
   test's deliberate miss ratio as a 10% error rate until
   `http.setResponseCallback(http.expectedStatuses(200, 404))` is set.

### Reproducing

Manifests, k6 scripts and the driver scripts used for this run are not
committed; they are reproducible from the description above. The shape is:
build the musl binary, generate and compile the dataset, mount both into a
stock image via `hostPath`, expose a `ClusterIP`, and drive it with
`grafana/k6` Jobs reading scripts from a ConfigMap.

## Notes

- `--no-metrics` disables per-request timing; see caveat 2 above — the
  difference was not measurable at this request rate on this hardware.
- Keys containing `/` are served via `/kv?k=...`; see the spec for why.
- These measurements were taken on a remote host by the controller running
  this task, not reproduced independently in this repository's own CI or
  development environment. Re-run and update this file after any change to
  the store or HTTP layer, or when moving to different hardware.
