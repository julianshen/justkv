use crate::store::csv_loader::LoadOptions;
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "justkv", about = "Read-only in-memory key-value server", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Load a dataset and serve it over HTTP
    Serve(ServeArgs),
    /// Validate a dataset and exit; reports every problem found
    Check(DataArgs),
    /// Compile a CSV/TSV dataset into the binary format
    Build(BuildArgs),
    /// Probe a running server's /health endpoint
    Healthcheck(HealthArgs),
}

/// Parsing flags shared by serve, check and build.
#[derive(Args, Debug, Clone)]
pub struct ParseArgs {
    /// Field delimiter; defaults to tab for .tsv, comma otherwise
    #[arg(long)]
    pub delimiter: Option<char>,
    /// Skip the first row
    #[arg(long)]
    pub header: bool,
    /// Permit values that are not valid UTF-8
    #[arg(long)]
    pub allow_binary: bool,
}

impl ParseArgs {
    pub fn load_options(&self, path: &Path) -> LoadOptions {
        LoadOptions {
            delimiter: self
                .delimiter
                .map(|c| c as u32 as u8)
                .unwrap_or_else(|| LoadOptions::delimiter_for_path(path)),
            has_header: self.header,
            allow_binary: self.allow_binary,
        }
    }
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Dataset to load: CSV/TSV, or a file produced by `justkv build`
    #[arg(long, env = "JUSTKV_DATA")]
    pub data: PathBuf,
    /// Address to listen on
    #[arg(long, env = "JUSTKV_BIND", default_value = "0.0.0.0:8080")]
    pub bind: String,
    /// Returned with 200 instead of 404 when a key is absent
    #[arg(long, env = "JUSTKV_DEFAULT")]
    pub default_value: Option<String>,
    /// Overrides the response Content-Type
    #[arg(long, env = "JUSTKV_CONTENT_TYPE")]
    pub content_type: Option<String>,
    /// Tokio worker threads; defaults to core count
    #[arg(long)]
    pub workers: Option<usize>,
    /// Skip per-request latency timing
    #[arg(long)]
    pub no_metrics: bool,
    #[command(flatten)]
    pub parse: ParseArgs,
}

#[derive(Args, Debug)]
pub struct DataArgs {
    /// Dataset to validate
    pub data: PathBuf,
    #[command(flatten)]
    pub parse: ParseArgs,
}

#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Source CSV/TSV dataset
    pub data: PathBuf,
    /// Destination for the compiled file
    #[arg(short, long)]
    pub out: PathBuf,
    #[command(flatten)]
    pub parse: ParseArgs,
}

#[derive(Args, Debug)]
pub struct HealthArgs {
    /// Address to probe; a 0.0.0.0 host is rewritten to 127.0.0.1
    #[arg(long, env = "JUSTKV_BIND", default_value = "0.0.0.0:8080")]
    pub bind: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::Path;

    #[test]
    fn serve_uses_documented_defaults() {
        let cli = Cli::try_parse_from(["justkv", "serve", "--data", "kv.csv"]).unwrap();
        let Command::Serve(a) = cli.command else { panic!("expected serve") };
        assert_eq!(a.bind, "0.0.0.0:8080");
        assert_eq!(a.data, Path::new("kv.csv"));
        assert!(a.default_value.is_none());
        assert!(a.content_type.is_none());
        assert!(!a.no_metrics);
        assert!(!a.parse.header);
        assert!(!a.parse.allow_binary);
    }

    #[test]
    fn serve_accepts_all_flags() {
        let cli = Cli::try_parse_from([
            "justkv", "serve", "--data", "d.tsv", "--bind", "127.0.0.1:9",
            "--default-value", "NA", "--content-type", "application/json",
            "--workers", "3", "--no-metrics", "--header", "--allow-binary",
            "--delimiter", ";",
        ]).unwrap();
        let Command::Serve(a) = cli.command else { panic!("expected serve") };
        assert_eq!(a.bind, "127.0.0.1:9");
        assert_eq!(a.default_value.as_deref(), Some("NA"));
        assert_eq!(a.content_type.as_deref(), Some("application/json"));
        assert_eq!(a.workers, Some(3));
        assert!(a.no_metrics && a.parse.header && a.parse.allow_binary);
        assert_eq!(a.parse.delimiter, Some(';'));
    }

    #[test]
    fn check_and_build_take_positional_data() {
        let cli = Cli::try_parse_from(["justkv", "check", "some.tsv"]).unwrap();
        let Command::Check(a) = cli.command else { panic!("expected check") };
        assert_eq!(a.data, Path::new("some.tsv"));

        let cli = Cli::try_parse_from(["justkv", "build", "in.csv", "-o", "out.bin"]).unwrap();
        let Command::Build(a) = cli.command else { panic!("expected build") };
        assert_eq!(a.data, Path::new("in.csv"));
        assert_eq!(a.out, Path::new("out.bin"));
    }

    #[test]
    fn load_options_infer_delimiter_from_extension() {
        let p = ParseArgs { delimiter: None, header: false, allow_binary: false };
        assert_eq!(p.load_options(Path::new("x.tsv")).delimiter, b'\t');
        assert_eq!(p.load_options(Path::new("x.csv")).delimiter, b',');
    }

    #[test]
    fn explicit_delimiter_overrides_extension() {
        let p = ParseArgs { delimiter: Some(';'), header: true, allow_binary: true };
        let o = p.load_options(Path::new("x.tsv"));
        assert_eq!(o.delimiter, b';');
        assert!(o.has_header && o.allow_binary);
    }

    #[test]
    fn healthcheck_defaults_to_serve_bind_default() {
        let cli = Cli::try_parse_from(["justkv", "healthcheck"]).unwrap();
        let Command::Healthcheck(a) = cli.command else { panic!("expected healthcheck") };
        assert_eq!(a.bind, "0.0.0.0:8080");
    }
}
