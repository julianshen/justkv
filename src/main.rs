use clap::Parser;
use justkv::config::{BuildArgs, Cli, Command, DataArgs, ServeArgs};
use justkv::metrics::Metrics;
use justkv::server::{AppState, DEFAULT_CONTENT_TYPE, run};
use justkv::store::compiled;
use justkv::store::csv_loader::parse_csv;
use justkv::store::{Loaded, LoadFailure, compiled::FLAG_BINARY, load};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

const EXIT_OK: u8 = 0;
const EXIT_INVALID_DATA: u8 = 1;
const EXIT_IO: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Check(args) => cmd_check(args),
        Command::Build(args) => cmd_build(args),
        Command::Serve(args) => cmd_serve(args),
        Command::Healthcheck(_) => {
            eprintln!("healthcheck is not implemented yet");
            EXIT_IO
        }
    };
    ExitCode::from(code)
}

/// Read and validate, reporting every problem. Returns the parsed parts so
/// `build` does not have to parse twice.
fn parse_validated(
    path: &std::path::Path,
    parse: &justkv::config::ParseArgs,
) -> Result<(Vec<u8>, Vec<justkv::store::Entry>), u8> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", path.display());
            return Err(EXIT_IO);
        }
    };
    let opts = parse.load_options(path);
    match parse_csv(&data, &opts) {
        Ok(parts) => Ok(parts),
        Err(errors) => {
            for e in &errors {
                eprintln!("{}: {e}", path.display());
            }
            eprintln!("{} problem(s) found in {}", errors.len(), path.display());
            Err(EXIT_INVALID_DATA)
        }
    }
}

fn cmd_check(args: DataArgs) -> u8 {
    match parse_validated(&args.data, &args.parse) {
        Ok((arena, entries)) => {
            println!(
                "ok: {} keys, {} bytes of data in {}",
                entries.len(),
                arena.len(),
                args.data.display()
            );
            EXIT_OK
        }
        Err(code) => code,
    }
}

fn cmd_build(args: BuildArgs) -> u8 {
    let (arena, entries) = match parse_validated(&args.data, &args.parse) {
        Ok(parts) => parts,
        Err(code) => return code,
    };
    let flags = if args.parse.allow_binary { compiled::FLAG_BINARY } else { 0 };

    // Write to a temporary sibling then rename, so a failed build never
    // leaves a half-written artifact that a later stage would happily load.
    let tmp = args.out.with_extension("tmp");
    let result = std::fs::File::create(&tmp).and_then(|f| {
        let mut w = std::io::BufWriter::new(f);
        compiled::write_compiled(&mut w, &arena, &entries, flags)?;
        std::io::Write::flush(&mut w)
    });
    if let Err(e) = result {
        eprintln!("error: cannot write {}: {e}", tmp.display());
        let _ = std::fs::remove_file(&tmp);
        return EXIT_IO;
    }
    if let Err(e) = std::fs::rename(&tmp, &args.out) {
        eprintln!("error: cannot finalize {}: {e}", args.out.display());
        let _ = std::fs::remove_file(&tmp);
        return EXIT_IO;
    }

    println!(
        "built {} ({} keys, {} bytes of data)",
        args.out.display(),
        entries.len(),
        arena.len()
    );
    EXIT_OK
}

fn cmd_serve(args: ServeArgs) -> u8 {
    let opts = args.parse.load_options(&args.data);
    let loaded: Loaded = match load(&args.data, &opts) {
        Ok(l) => l,
        Err(LoadFailure::Io(e)) => {
            eprintln!("error: cannot read {}: {e}", args.data.display());
            return EXIT_IO;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_INVALID_DATA;
        }
    };

    // A compiled file built with --allow-binary carries the flag, so a binary
    // dataset stays correctly typed without re-passing the flag at serve time.
    let binary = loaded.flags & FLAG_BINARY != 0;
    let content_type = args.content_type.clone().unwrap_or_else(|| {
        if binary {
            "application/octet-stream".to_string()
        } else {
            DEFAULT_CONTENT_TYPE.to_string()
        }
    });
    let content_type = match axum::http::HeaderValue::from_str(&content_type) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("error: invalid --content-type value: {content_type:?}");
            return EXIT_IO;
        }
    };

    let state = Arc::new(AppState {
        keys: loaded.store.len(),
        arena_bytes: loaded.store.arena_len(),
        source: loaded.source.display().to_string(),
        format: loaded.format.as_str(),
        load_ms: loaded.load_duration.as_secs_f64() * 1000.0,
        store: Arc::new(loaded.store),
        metrics: Arc::new(Metrics::new()),
        default_value: args.default_value.map(|s| bytes::Bytes::from(s.into_bytes())),
        content_type,
        started: Instant::now(),
        timing: !args.no_metrics,
    });

    let mut rt = tokio::runtime::Builder::new_multi_thread();
    rt.enable_all();
    if let Some(w) = args.workers {
        rt.worker_threads(w.max(1));
    }
    let rt = match rt.build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot start runtime: {e}");
            return EXIT_IO;
        }
    };
    match rt.block_on(run(state, &args.bind)) {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_IO
        }
    }
}
