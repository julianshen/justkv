# justkv benchmarks

Reproducible measurement of the throughput and memory-efficiency requirements
from the spec. Re-run after any change to the store or HTTP layer.

Numbers in this file are measured on a remote x86_64 Linux host (the local
development machine here does not have the disk headroom for a 1M-row
dataset plus a release build). Every value cell below is either an observed
measurement or explicitly marked `_pending measurement_` — none are
estimated, extrapolated, or guessed.

## Setup

    cargo run --release --example gen -- 1000000 bench/kv.tsv
    cargo run --release -- build bench/kv.tsv -o bench/kv.bin
    cargo run --release -- serve --data bench/kv.bin --bind 127.0.0.1:8200

## Load

    oha -z 30s -c 50 --no-tui http://127.0.0.1:8200/kv/key500000

## Results

| Measurement | Value |
|---|---|
| Machine | _pending measurement_ |
| Rows | 1,000,000 |
| Source TSV size | _pending measurement_ |
| Compiled size | _pending measurement_ |
| RSS after load | _pending measurement_ |
| RSS overhead vs data | _pending measurement_ |
| Startup, CSV | _pending measurement_ |
| Startup, compiled | _pending measurement_ |
| Throughput | _pending measurement_ |
| p50 / p99 latency | _pending measurement_ |
| Image size | _pending measurement_ |

## Notes

- `--no-metrics` disables per-request timing; compare against the figures above
  to see what observability costs on this hardware.
- Keys containing `/` are served via `/kv?k=...`; see the spec for why.
- Measurements above are pending a run on a host with sufficient disk (this
  worktree has ~1 GiB free, insufficient for a 1,000,000-row dataset plus a
  release build). Do not fill in plausible-looking numbers here — replace
  `_pending measurement_` only with values actually observed from a real run.
