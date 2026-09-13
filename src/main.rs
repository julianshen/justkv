use clap::Parser;
use justkv::config::{BuildArgs, Cli, Command, DataArgs};
use justkv::store::compiled;
use justkv::store::csv_loader::parse_csv;
use std::process::ExitCode;

const EXIT_OK: u8 = 0;
const EXIT_INVALID_DATA: u8 = 1;
const EXIT_IO: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Check(args) => cmd_check(args),
        Command::Build(args) => cmd_build(args),
        Command::Serve(_) => {
            eprintln!("serve is not implemented yet");
            EXIT_IO
        }
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
