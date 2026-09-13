use super::Entry;
use foldhash::fast::RandomState;
use hashbrown::HashTable;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct LoadOptions {
    pub delimiter: u8,
    pub has_header: bool,
    pub allow_binary: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions { delimiter: b',', has_header: false, allow_binary: false }
    }
}

impl LoadOptions {
    /// `.tsv` means tab; everything else means comma. Explicit `--delimiter`
    /// overrides this at the config layer.
    pub fn delimiter_for_path(path: &Path) -> u8 {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("tsv") => b'\t',
            _ => b',',
        }
    }
}

#[derive(Clone, Debug)]
pub enum DataErrorKind {
    WrongColumnCount { found: usize },
    DuplicateKey { key: String, first_line: u64 },
    InvalidUtf8Value,
    Malformed(String),
    TooLarge,
}

#[derive(Clone, Debug)]
pub struct DataError {
    pub line: u64,
    pub kind: DataErrorKind,
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: ", self.line)?;
        match &self.kind {
            DataErrorKind::WrongColumnCount { found } => {
                write!(f, "expected 2 columns, found {found}")
            }
            DataErrorKind::DuplicateKey { key, first_line } => {
                write!(f, "duplicate key {key:?}, first seen on line {first_line}")
            }
            DataErrorKind::InvalidUtf8Value => {
                write!(f, "value is not valid UTF-8 (pass --allow-binary to permit)")
            }
            DataErrorKind::Malformed(msg) => write!(f, "malformed row: {msg}"),
            DataErrorKind::TooLarge => {
                write!(f, "dataset exceeds the 4 GiB limit imposed by u32 offsets")
            }
        }
    }
}

impl std::error::Error for DataError {}

#[inline]
fn hash_bytes(state: &RandomState, b: &[u8]) -> u64 {
    let mut h = state.build_hasher();
    h.write(b);
    h.finish()
}

/// Parse CSV/TSV into a packed arena plus entry list.
///
/// Returns *every* problem found rather than stopping at the first, because
/// this runs at build time and a caller fixing their data wants the whole list.
///
/// Duplicate detection reuses the arena rather than a `HashMap<Vec<u8>, _>`:
/// copying every key would cost tens of megabytes at 1M keys and defeat the
/// point of the packed layout. Instead we index entry positions and compare
/// against arena slices.
pub fn parse_csv(
    data: &[u8],
    opts: &LoadOptions,
) -> Result<(Vec<u8>, Vec<Entry>), Vec<DataError>> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(opts.delimiter)
        .has_headers(opts.has_header)
        .flexible(true) // count columns ourselves so we can report the line
        .from_reader(data);

    let mut arena: Vec<u8> = Vec::with_capacity(data.len());
    let mut entries: Vec<Entry> = Vec::new();
    let mut lines: Vec<u64> = Vec::new();
    let mut errors: Vec<DataError> = Vec::new();

    let state = RandomState::default();
    let mut seen: HashTable<u32> = HashTable::new();
    let mut rec = csv::ByteRecord::new();

    loop {
        let line_hint = rec.position().map(|p| p.line()).unwrap_or(0) + 1;
        match rdr.read_byte_record(&mut rec) {
            Ok(false) => break,
            Ok(true) => {}
            Err(e) => {
                errors.push(DataError {
                    line: line_hint,
                    kind: DataErrorKind::Malformed(e.to_string()),
                });
                break; // reader state is unreliable after a parse error
            }
        }

        let line = rec.position().map(|p| p.line()).unwrap_or(line_hint);

        if rec.len() != 2 {
            errors.push(DataError {
                line,
                kind: DataErrorKind::WrongColumnCount { found: rec.len() },
            });
            continue;
        }

        let key = &rec[0];
        let val = &rec[1];

        let kh = hash_bytes(&state, key);
        let dup = seen.find(kh, |&i| {
            let e = &entries[i as usize];
            &arena[e.k_off as usize..(e.k_off + e.k_len) as usize] == key
        });
        if let Some(&idx) = dup {
            errors.push(DataError {
                line,
                kind: DataErrorKind::DuplicateKey {
                    key: String::from_utf8_lossy(key).into_owned(),
                    first_line: lines[idx as usize],
                },
            });
            continue;
        }

        if !opts.allow_binary && std::str::from_utf8(val).is_err() {
            errors.push(DataError { line, kind: DataErrorKind::InvalidUtf8Value });
            continue;
        }

        if arena.len() + key.len() + val.len() > u32::MAX as usize {
            errors.push(DataError { line, kind: DataErrorKind::TooLarge });
            break;
        }

        let k_off = arena.len() as u32;
        arena.extend_from_slice(key);
        let v_off = arena.len() as u32;
        arena.extend_from_slice(val);

        let idx = entries.len() as u32;
        entries.push(Entry {
            k_off,
            k_len: key.len() as u32,
            v_off,
            v_len: val.len() as u32,
        });
        lines.push(line);
        seen.insert_unique(kh, idx, |&i| {
            let e = &entries[i as usize];
            hash_bytes(&state, &arena[e.k_off as usize..(e.k_off + e.k_len) as usize])
        });
    }

    if errors.is_empty() {
        arena.shrink_to_fit();
        Ok((arena, entries))
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse_ok(data: &[u8], opts: &LoadOptions) -> crate::store::Store {
        let (arena, entries) = parse_csv(data, opts).expect("expected clean parse");
        crate::store::Store::from_parts(arena, entries)
    }

    #[test]
    fn parses_simple_comma_rows() {
        let s = parse_ok(b"a,1\nb,2\n", &LoadOptions::default());
        assert_eq!(s.len(), 2);
        assert_eq!(s.get(b"a").as_deref(), Some(&b"1"[..]));
        assert_eq!(s.get(b"b").as_deref(), Some(&b"2"[..]));
    }

    #[test]
    fn infers_tab_delimiter_from_tsv_extension() {
        assert_eq!(LoadOptions::delimiter_for_path(Path::new("kv.tsv")), b'\t');
        assert_eq!(LoadOptions::delimiter_for_path(Path::new("kv.csv")), b',');
        assert_eq!(LoadOptions::delimiter_for_path(Path::new("kv.dat")), b',');
        assert_eq!(LoadOptions::delimiter_for_path(Path::new("KV.TSV")), b'\t');
    }

    #[test]
    fn parses_tab_separated_values() {
        let opts = LoadOptions { delimiter: b'\t', ..LoadOptions::default() };
        let s = parse_ok(b"a\t1\nb\t2\n", &opts);
        assert_eq!(s.get(b"a").as_deref(), Some(&b"1"[..]));
    }

    #[test]
    fn decodes_rfc4180_quoting_including_escaped_quotes() {
        // value is:  he said "hi", ok
        let s = parse_ok(b"k,\"he said \"\"hi\"\", ok\"\n", &LoadOptions::default());
        assert_eq!(s.get(b"k").as_deref(), Some(&b"he said \"hi\", ok"[..]));
    }

    #[test]
    fn decodes_embedded_newline_in_quoted_value() {
        let s = parse_ok(b"k,\"line1\nline2\"\n", &LoadOptions::default());
        assert_eq!(s.get(b"k").as_deref(), Some(&b"line1\nline2"[..]));
    }

    #[test]
    fn accepts_empty_value_and_empty_key() {
        let s = parse_ok(b"k,\n,v\n", &LoadOptions::default());
        assert_eq!(s.get(b"k").as_deref(), Some(&b""[..]));
        assert_eq!(s.get(b"").as_deref(), Some(&b"v"[..]));
    }

    #[test]
    fn skips_header_row_when_requested() {
        let opts = LoadOptions { has_header: true, ..LoadOptions::default() };
        let s = parse_ok(b"key,value\na,1\n", &opts);
        assert_eq!(s.len(), 1);
        assert!(s.get(b"key").is_none());
        assert_eq!(s.get(b"a").as_deref(), Some(&b"1"[..]));
    }

    #[test]
    fn duplicate_key_reports_key_and_both_line_numbers() {
        let errs = parse_csv(b"a,1\nb,2\na,3\n", &LoadOptions::default()).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].line, 3);
        match &errs[0].kind {
            DataErrorKind::DuplicateKey { key, first_line } => {
                assert_eq!(key, "a");
                assert_eq!(*first_line, 1);
            }
            other => panic!("expected DuplicateKey, got {other:?}"),
        }
    }

    #[test]
    fn wrong_column_count_reports_line_and_count() {
        let errs = parse_csv(b"a,1\nb,2,3\n", &LoadOptions::default()).unwrap_err();
        assert_eq!(errs[0].line, 2);
        assert!(matches!(errs[0].kind, DataErrorKind::WrongColumnCount { found: 3 }));
    }

    #[test]
    fn collects_every_error_not_just_the_first() {
        let errs = parse_csv(b"a,1\nb\nc,2,3\na,9\n", &LoadOptions::default()).unwrap_err();
        assert_eq!(errs.len(), 3, "got: {errs:?}");
        assert_eq!(errs.iter().map(|e| e.line).collect::<Vec<_>>(), vec![2, 3, 4]);
    }

    #[test]
    fn rejects_non_utf8_value_by_default() {
        let errs = parse_csv(b"k,\xff\xfe\n", &LoadOptions::default()).unwrap_err();
        assert!(matches!(errs[0].kind, DataErrorKind::InvalidUtf8Value));
    }

    #[test]
    fn accepts_non_utf8_value_when_allow_binary() {
        let opts = LoadOptions { allow_binary: true, ..LoadOptions::default() };
        let s = parse_ok(b"k,\xff\xfe\n", &opts);
        assert_eq!(s.get(b"k").as_deref(), Some(&b"\xff\xfe"[..]));
    }

    #[test]
    fn error_display_mentions_line_number() {
        let errs = parse_csv(b"a,1\na,2\n", &LoadOptions::default()).unwrap_err();
        assert!(errs[0].to_string().contains("line 2"), "got: {}", errs[0]);
    }
}
