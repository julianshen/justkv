pub mod csv_loader;

use bytes::Bytes;
use foldhash::fast::RandomState;
use hashbrown::HashTable;
use std::hash::{BuildHasher, Hasher};

/// A key/value pair as a pair of (offset, length) windows into the arena.
/// Fixed 16 bytes, so 1M entries cost 16MB regardless of key/value sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub k_off: u32,
    pub k_len: u32,
    pub v_off: u32,
    pub v_len: u32,
}

impl Entry {
    #[inline]
    fn key<'a>(&self, arena: &'a [u8]) -> &'a [u8] {
        &arena[self.k_off as usize..(self.k_off + self.k_len) as usize]
    }
}

/// Immutable key/value store. Read-only by construction, so the read path
/// needs no synchronization — share it with `Arc` and read from any thread.
pub struct Store {
    arena: Bytes,
    table: HashTable<Entry>,
    hasher: RandomState,
}

#[inline]
fn hash_key(state: &RandomState, key: &[u8]) -> u64 {
    let mut h = state.build_hasher();
    h.write(key);
    h.finish()
}

impl Store {
    /// Build a store from a packed arena and its entry list.
    ///
    /// Entries are assumed already validated (no duplicates, offsets in
    /// range) — CSV validation lives in `csv_loader`, and the compiled
    /// format is only ever produced from validated input.
    pub fn from_parts(arena: Vec<u8>, entries: Vec<Entry>) -> Store {
        let hasher = RandomState::default();
        let arena = Bytes::from(arena);
        let mut table = HashTable::with_capacity(entries.len());
        for e in entries {
            let h = hash_key(&hasher, e.key(&arena));
            table.insert_unique(h, e, |other| hash_key(&hasher, other.key(&arena)));
        }
        Store { arena, table, hasher }
    }

    /// Look up a key. The returned `Bytes` points into the shared arena —
    /// this is a refcount increment, not a copy, so value size is free.
    #[inline]
    pub fn get(&self, key: &[u8]) -> Option<Bytes> {
        let h = hash_key(&self.hasher, key);
        let e = self.table.find(h, |e| e.key(&self.arena) == key)?;
        Some(self.arena.slice(e.v_off as usize..(e.v_off + e.v_len) as usize))
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub fn arena_len(&self) -> usize {
        self.arena.len()
    }

    pub fn arena(&self) -> &[u8] {
        &self.arena
    }
}

pub mod compiled;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::store::compiled::CompiledError;
use crate::store::csv_loader::{DataError, LoadOptions, parse_csv};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    Csv,
    Compiled,
}

impl SourceFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceFormat::Csv => "csv",
            SourceFormat::Compiled => "compiled",
        }
    }
}

pub struct Loaded {
    pub store: Store,
    pub format: SourceFormat,
    pub flags: u32,
    pub load_duration: Duration,
    pub source: PathBuf,
}

#[derive(Debug)]
pub enum LoadFailure {
    Io(std::io::Error),
    Data(Vec<DataError>),
    Compiled(CompiledError),
}

impl std::fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadFailure::Io(e) => write!(f, "{e}"),
            LoadFailure::Compiled(e) => write!(f, "{e}"),
            LoadFailure::Data(errs) => {
                for e in errs {
                    writeln!(f, "{e}")?;
                }
                write!(f, "{} problem(s) found", errs.len())
            }
        }
    }
}

impl std::error::Error for LoadFailure {}

/// Load a dataset, choosing the loader by magic bytes so one binary serves
/// both a compiled artifact in production and a raw CSV in development.
pub fn load(path: &Path, opts: &LoadOptions) -> Result<Loaded, LoadFailure> {
    let started = Instant::now();
    let data = std::fs::read(path).map_err(LoadFailure::Io)?;

    let (arena, entries, format, flags) = if compiled::is_compiled(&data) {
        let (a, e, f) = compiled::read_compiled(&data).map_err(LoadFailure::Compiled)?;
        (a, e, SourceFormat::Compiled, f)
    } else {
        let (a, e) = parse_csv(&data, opts).map_err(LoadFailure::Data)?;
        let f = if opts.allow_binary { compiled::FLAG_BINARY } else { 0 };
        (a, e, SourceFormat::Csv, f)
    };

    drop(data); // release the file buffer before building the table
    let store = Store::from_parts(arena, entries);
    Ok(Loaded {
        store,
        format,
        flags,
        load_duration: started.elapsed(),
        source: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Store {
        // "akey"+"aval"+"bkey"+"bval"
        let arena = b"akeyavalbkeybval".to_vec();
        let entries = vec![
            Entry { k_off: 0, k_len: 4, v_off: 4, v_len: 4 },
            Entry { k_off: 8, k_len: 4, v_off: 12, v_len: 4 },
        ];
        Store::from_parts(arena, entries)
    }

    #[test]
    fn get_returns_value_for_present_key() {
        let s = sample();
        assert_eq!(s.get(b"akey").as_deref(), Some(&b"aval"[..]));
        assert_eq!(s.get(b"bkey").as_deref(), Some(&b"bval"[..]));
    }

    #[test]
    fn get_returns_none_for_absent_key() {
        assert!(sample().get(b"zzzz").is_none());
    }

    #[test]
    fn get_does_not_match_on_prefix_or_substring() {
        let s = sample();
        assert!(s.get(b"ake").is_none());
        assert!(s.get(b"akeyaval").is_none());
    }

    #[test]
    fn empty_key_and_empty_value_are_representable() {
        let arena = b"x".to_vec();
        // key "" -> value "x", and key "x" -> value ""
        let entries = vec![
            Entry { k_off: 0, k_len: 0, v_off: 0, v_len: 1 },
            Entry { k_off: 0, k_len: 1, v_off: 0, v_len: 0 },
        ];
        let s = Store::from_parts(arena, entries);
        assert_eq!(s.get(b"").as_deref(), Some(&b"x"[..]));
        assert_eq!(s.get(b"x").as_deref(), Some(&b""[..]));
    }

    #[test]
    fn reports_len_and_arena_len() {
        let s = sample();
        assert_eq!(s.len(), 2);
        assert_eq!(s.arena_len(), 16);
    }
}
