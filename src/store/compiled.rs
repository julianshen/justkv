use super::Entry;
use std::io::Write;

pub const MAGIC: [u8; 8] = *b"JUSTKV01";
pub const HEADER_LEN: usize = 24;
pub const ENTRY_LEN: usize = 16;

/// Values were not UTF-8 validated; the server defaults to octet-stream.
pub const FLAG_BINARY: u32 = 1;

#[derive(Debug)]
pub enum CompiledError {
    BadMagic,
    Truncated,
    OutOfBounds { index: usize },
}

impl std::fmt::Display for CompiledError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompiledError::BadMagic => write!(f, "not a justkv compiled file (bad magic)"),
            CompiledError::Truncated => write!(f, "compiled file is truncated"),
            CompiledError::OutOfBounds { index } => {
                write!(f, "entry {index} points outside the arena")
            }
        }
    }
}

impl std::error::Error for CompiledError {}

pub fn is_compiled(data: &[u8]) -> bool {
    data.len() >= MAGIC.len() && data[..MAGIC.len()] == MAGIC
}

pub fn write_compiled<W: Write>(
    w: &mut W,
    arena: &[u8],
    entries: &[Entry],
    flags: u32,
) -> std::io::Result<()> {
    w.write_all(&MAGIC)?;
    w.write_all(&flags.to_le_bytes())?;
    w.write_all(&(entries.len() as u32).to_le_bytes())?;
    w.write_all(&(arena.len() as u32).to_le_bytes())?;
    w.write_all(&0u32.to_le_bytes())?; // reserved
    for e in entries {
        w.write_all(&e.k_off.to_le_bytes())?;
        w.write_all(&e.k_len.to_le_bytes())?;
        w.write_all(&e.v_off.to_le_bytes())?;
        w.write_all(&e.v_len.to_le_bytes())?;
    }
    w.write_all(arena)?;
    Ok(())
}

#[inline]
fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Read the compiled format. Integers are decoded byte-by-byte rather than
/// reinterpreted from the buffer, which sidesteps both alignment and
/// endianness hazards. Entry bounds are validated because a corrupt or
/// hand-edited file must not produce out-of-range slicing later.
pub fn read_compiled(data: &[u8]) -> Result<(Vec<u8>, Vec<Entry>, u32), CompiledError> {
    if !is_compiled(data) {
        return Err(if data.len() < MAGIC.len() {
            CompiledError::Truncated
        } else {
            CompiledError::BadMagic
        });
    }
    if data.len() < HEADER_LEN {
        return Err(CompiledError::Truncated);
    }

    let flags = le_u32(&data[8..12]);
    let count = le_u32(&data[12..16]) as usize;
    let arena_len = le_u32(&data[16..20]) as usize;

    let entries_end = HEADER_LEN
        .checked_add(count.checked_mul(ENTRY_LEN).ok_or(CompiledError::Truncated)?)
        .ok_or(CompiledError::Truncated)?;
    let total = entries_end.checked_add(arena_len).ok_or(CompiledError::Truncated)?;
    if data.len() < total {
        return Err(CompiledError::Truncated);
    }

    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let o = HEADER_LEN + i * ENTRY_LEN;
        let e = Entry {
            k_off: le_u32(&data[o..o + 4]),
            k_len: le_u32(&data[o + 4..o + 8]),
            v_off: le_u32(&data[o + 8..o + 12]),
            v_len: le_u32(&data[o + 12..o + 16]),
        };
        let k_end = e.k_off as usize + e.k_len as usize;
        let v_end = e.v_off as usize + e.v_len as usize;
        if k_end > arena_len || v_end > arena_len {
            return Err(CompiledError::OutOfBounds { index: i });
        }
        entries.push(e);
    }

    let arena = data[entries_end..total].to_vec();
    Ok((arena, entries, flags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Entry;

    fn fixture() -> (Vec<u8>, Vec<Entry>) {
        (
            b"akeyavalbkeybval".to_vec(),
            vec![
                Entry { k_off: 0, k_len: 4, v_off: 4, v_len: 4 },
                Entry { k_off: 8, k_len: 4, v_off: 12, v_len: 4 },
            ],
        )
    }

    #[test]
    fn round_trips_arena_entries_and_flags() {
        let (arena, entries) = fixture();
        let mut buf = Vec::new();
        write_compiled(&mut buf, &arena, &entries, FLAG_BINARY).unwrap();
        let (a2, e2, flags) = read_compiled(&buf).unwrap();
        assert_eq!(a2, arena);
        assert_eq!(e2, entries);
        assert_eq!(flags, FLAG_BINARY);
    }

    #[test]
    fn written_header_has_expected_layout() {
        let (arena, entries) = fixture();
        let mut buf = Vec::new();
        write_compiled(&mut buf, &arena, &entries, 0).unwrap();
        assert_eq!(&buf[0..8], &MAGIC);
        assert_eq!(u32::from_le_bytes(buf[8..12].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(buf[12..16].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(buf[16..20].try_into().unwrap()), 16);
        assert_eq!(buf.len(), 24 + 16 * 2 + 16);
    }

    #[test]
    fn round_trips_empty_dataset() {
        let mut buf = Vec::new();
        write_compiled(&mut buf, &[], &[], 0).unwrap();
        let (a, e, f) = read_compiled(&buf).unwrap();
        assert!(a.is_empty() && e.is_empty() && f == 0);
    }

    #[test]
    fn is_compiled_detects_magic() {
        let (arena, entries) = fixture();
        let mut buf = Vec::new();
        write_compiled(&mut buf, &arena, &entries, 0).unwrap();
        assert!(is_compiled(&buf));
        assert!(!is_compiled(b"a,1\nb,2\n"));
        assert!(!is_compiled(b"JUS"));
        assert!(!is_compiled(b""));
    }

    #[test]
    fn rejects_bad_magic() {
        let err = read_compiled(b"NOTMAGIC________________").unwrap_err();
        assert!(matches!(err, CompiledError::BadMagic));
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(matches!(read_compiled(b"JUSTKV01").unwrap_err(), CompiledError::Truncated));
    }

    #[test]
    fn rejects_truncated_body() {
        let (arena, entries) = fixture();
        let mut buf = Vec::new();
        write_compiled(&mut buf, &arena, &entries, 0).unwrap();
        buf.truncate(buf.len() - 4);
        assert!(matches!(read_compiled(&buf).unwrap_err(), CompiledError::Truncated));
    }

    #[test]
    fn rejects_entry_pointing_outside_arena() {
        let arena = b"abc".to_vec();
        let entries = vec![Entry { k_off: 0, k_len: 1, v_off: 1, v_len: 99 }];
        let mut buf = Vec::new();
        write_compiled(&mut buf, &arena, &entries, 0).unwrap();
        assert!(matches!(read_compiled(&buf).unwrap_err(), CompiledError::OutOfBounds { index: 0 }));
    }
}
