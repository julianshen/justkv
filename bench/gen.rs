//! Generate a synthetic dataset for benchmarking.
//!
//! Usage: cargo run --release --example gen -- <rows> <out.tsv>

use std::io::{BufWriter, Write};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let rows: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let out = args.get(2).map(String::as_str).unwrap_or("bench/kv.tsv");

    let f = std::fs::File::create(out)?;
    let mut w = BufWriter::new(f);
    // Deterministic pseudo-random values so runs are comparable.
    let mut seed: u64 = 0x9E3779B97F4A7C15;
    for i in 0..rows {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        writeln!(w, "key{i}\tvalue-{i}-{seed:016x}")?;
    }
    w.flush()?;
    eprintln!("wrote {rows} rows to {out}");
    Ok(())
}
