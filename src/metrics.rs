use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

/// Upper bounds in seconds. A 13th implicit bucket catches everything above.
pub const BUCKETS: [f64; 12] = [
    0.000025, 0.00005, 0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Hit,
    /// Key absent and no default configured — a 404.
    Miss,
    /// Key absent but a default was served — still a miss, served as 200.
    Default,
}

/// Plain atomics rather than a metrics crate: nine numbers and a fixed
/// histogram render in a few dozen lines and drop a dependency tree.
///
/// Everything is `Relaxed`. These counters are for observation, not for
/// ordering other memory, so stronger orderings would only cost throughput.
pub struct Metrics {
    requests: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    defaults: AtomicU64,
    bytes: AtomicU64,
    buckets: [AtomicU64; BUCKETS.len() + 1],
    sum_nanos: AtomicU64,
    timed: AtomicU64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Metrics {
        Metrics {
            requests: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            defaults: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_nanos: AtomicU64::new(0),
            timed: AtomicU64::new(0),
        }
    }

    pub fn record(&self, outcome: Outcome, bytes: usize, elapsed: Option<Duration>) {
        self.requests.fetch_add(1, Relaxed);
        self.bytes.fetch_add(bytes as u64, Relaxed);
        match outcome {
            Outcome::Hit => {
                self.hits.fetch_add(1, Relaxed);
            }
            Outcome::Miss => {
                self.misses.fetch_add(1, Relaxed);
            }
            Outcome::Default => {
                // Deliberate: the status code says 200, the metrics say miss.
                self.misses.fetch_add(1, Relaxed);
                self.defaults.fetch_add(1, Relaxed);
            }
        }
        if let Some(d) = elapsed {
            let secs = d.as_secs_f64();
            let idx = BUCKETS.iter().position(|&b| secs <= b).unwrap_or(BUCKETS.len());
            self.buckets[idx].fetch_add(1, Relaxed);
            self.sum_nanos.fetch_add(d.as_nanos() as u64, Relaxed);
            self.timed.fetch_add(1, Relaxed);
        }
    }

    pub fn render(&self, keys: usize, arena_bytes: usize, uptime: Duration) -> String {
        let mut o = String::with_capacity(1024);

        let counter = |o: &mut String, name: &str, help: &str, v: u64| {
            let _ = writeln!(o, "# HELP {name} {help}");
            let _ = writeln!(o, "# TYPE {name} counter");
            let _ = writeln!(o, "{name} {v}");
        };
        let gauge = |o: &mut String, name: &str, help: &str, v: u64| {
            let _ = writeln!(o, "# HELP {name} {help}");
            let _ = writeln!(o, "# TYPE {name} gauge");
            let _ = writeln!(o, "{name} {v}");
        };

        counter(&mut o, "justkv_requests_total", "Total key lookups served.", self.requests.load(Relaxed));
        counter(&mut o, "justkv_hits_total", "Lookups that found a key.", self.hits.load(Relaxed));
        counter(&mut o, "justkv_misses_total", "Lookups with no matching key, including those served a default.", self.misses.load(Relaxed));
        counter(&mut o, "justkv_defaults_served_total", "Misses answered with the configured default value.", self.defaults.load(Relaxed));
        counter(&mut o, "justkv_response_bytes_total", "Value bytes written to clients.", self.bytes.load(Relaxed));

        let _ = writeln!(o, "# HELP justkv_request_duration_seconds Lookup handling latency.");
        let _ = writeln!(o, "# TYPE justkv_request_duration_seconds histogram");
        let mut cumulative = 0u64;
        for (i, b) in BUCKETS.iter().enumerate() {
            cumulative += self.buckets[i].load(Relaxed);
            let _ = writeln!(o, "justkv_request_duration_seconds_bucket{{le=\"{b}\"}} {cumulative}");
        }
        cumulative += self.buckets[BUCKETS.len()].load(Relaxed);
        let _ = writeln!(o, "justkv_request_duration_seconds_bucket{{le=\"+Inf\"}} {cumulative}");
        let sum = self.sum_nanos.load(Relaxed) as f64 / 1e9;
        let _ = writeln!(o, "justkv_request_duration_seconds_sum {sum}");
        let _ = writeln!(o, "justkv_request_duration_seconds_count {}", self.timed.load(Relaxed));

        gauge(&mut o, "justkv_keys", "Keys loaded.", keys as u64);
        gauge(&mut o, "justkv_arena_bytes", "Bytes of key and value data held in memory.", arena_bytes as u64);
        gauge(&mut o, "justkv_uptime_seconds", "Seconds since the server finished loading.", uptime.as_secs());

        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn line<'a>(out: &'a str, name: &str) -> &'a str {
        out.lines()
            .find(|l| l.starts_with(name) && !l.starts_with('#'))
            .unwrap_or_else(|| panic!("missing metric {name} in:\n{out}"))
    }

    #[test]
    fn counts_hits_misses_and_defaults_separately() {
        let m = Metrics::new();
        m.record(Outcome::Hit, 10, None);
        m.record(Outcome::Miss, 0, None);
        m.record(Outcome::Default, 3, None);
        let out = m.render(2, 16, Duration::from_secs(5));
        assert_eq!(line(&out, "justkv_requests_total"), "justkv_requests_total 3");
        assert_eq!(line(&out, "justkv_hits_total"), "justkv_hits_total 1");
        assert_eq!(line(&out, "justkv_response_bytes_total"), "justkv_response_bytes_total 13");
    }

    #[test]
    fn a_default_served_response_counts_as_a_miss_not_a_hit() {
        let m = Metrics::new();
        m.record(Outcome::Default, 3, None);
        let out = m.render(0, 0, Duration::from_secs(1));
        assert_eq!(line(&out, "justkv_misses_total"), "justkv_misses_total 1");
        assert_eq!(line(&out, "justkv_hits_total"), "justkv_hits_total 0");
        assert_eq!(line(&out, "justkv_defaults_served_total"), "justkv_defaults_served_total 1");
    }

    #[test]
    fn renders_gauges_from_arguments() {
        let out = Metrics::new().render(42, 4096, Duration::from_secs(7));
        assert_eq!(line(&out, "justkv_keys"), "justkv_keys 42");
        assert_eq!(line(&out, "justkv_arena_bytes"), "justkv_arena_bytes 4096");
        assert_eq!(line(&out, "justkv_uptime_seconds"), "justkv_uptime_seconds 7");
    }

    #[test]
    fn histogram_buckets_are_cumulative_and_end_with_inf() {
        let m = Metrics::new();
        m.record(Outcome::Hit, 1, Some(Duration::from_micros(30)));   // > 25us
        m.record(Outcome::Hit, 1, Some(Duration::from_micros(200)));  // > 100us
        let out = m.render(0, 0, Duration::from_secs(1));
        assert!(out.contains(r#"justkv_request_duration_seconds_bucket{le="0.000025"} 0"#), "{out}");
        assert!(out.contains(r#"justkv_request_duration_seconds_bucket{le="0.00005"} 1"#), "{out}");
        assert!(out.contains(r#"justkv_request_duration_seconds_bucket{le="0.00025"} 2"#), "{out}");
        assert!(out.contains(r#"justkv_request_duration_seconds_bucket{le="+Inf"} 2"#), "{out}");
        assert!(out.contains("justkv_request_duration_seconds_count 2"), "{out}");
    }

    #[test]
    fn untimed_requests_are_excluded_from_the_histogram() {
        let m = Metrics::new();
        m.record(Outcome::Hit, 1, None);
        let out = m.render(0, 0, Duration::from_secs(1));
        assert!(out.contains("justkv_request_duration_seconds_count 0"), "{out}");
        assert_eq!(line(&out, "justkv_requests_total"), "justkv_requests_total 1");
    }

    #[test]
    fn output_declares_help_and_type_for_every_metric() {
        let out = Metrics::new().render(0, 0, Duration::from_secs(1));
        for name in [
            "justkv_requests_total", "justkv_hits_total", "justkv_misses_total",
            "justkv_defaults_served_total", "justkv_response_bytes_total",
            "justkv_request_duration_seconds", "justkv_keys",
            "justkv_arena_bytes", "justkv_uptime_seconds",
        ] {
            assert!(out.contains(&format!("# TYPE {name} ")), "missing TYPE for {name}:\n{out}");
            assert!(out.contains(&format!("# HELP {name} ")), "missing HELP for {name}:\n{out}");
        }
    }
}
