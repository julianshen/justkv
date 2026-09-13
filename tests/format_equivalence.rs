use proptest::prelude::*;

/// Render key/value pairs as RFC 4180 CSV, quoting every field so that
/// commas, quotes and newlines survive the round trip.
fn to_csv(pairs: &[(String, String)]) -> Vec<u8> {
    let mut out = String::new();
    for (k, v) in pairs {
        out.push('"');
        out.push_str(&k.replace('"', "\"\""));
        out.push_str("\",\"");
        out.push_str(&v.replace('"', "\"\""));
        out.push_str("\"\n");
    }
    out.into_bytes()
}

/// Distinct keys only — duplicates are a validation error, tested elsewhere.
fn unique_pairs() -> impl Strategy<Value = Vec<(String, String)>> {
    prop::collection::vec((".{0,12}", ".{0,24}"), 0..40).prop_map(|v| {
        let mut seen = std::collections::HashSet::new();
        v.into_iter().filter(|(k, _)| seen.insert(k.clone())).collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// The two load paths must be indistinguishable: whatever the CSV loader
    /// produces, compiling and reloading it must yield the same store.
    #[test]
    fn csv_and_compiled_paths_agree(pairs in unique_pairs()) {
        let csv = to_csv(&pairs);
        let opts = justkv::store::csv_loader::LoadOptions::default();

        let (arena, entries) = justkv::store::csv_loader::parse_csv(&csv, &opts)
            .expect("generated CSV must parse cleanly");

        let mut buf = Vec::new();
        justkv::store::compiled::write_compiled(&mut buf, &arena, &entries, 0).unwrap();
        let (arena2, entries2, _) = justkv::store::compiled::read_compiled(&buf).unwrap();

        prop_assert_eq!(&arena, &arena2);
        prop_assert_eq!(&entries, &entries2);

        let a = justkv::store::Store::from_parts(arena, entries);
        let b = justkv::store::Store::from_parts(arena2, entries2);
        prop_assert_eq!(a.len(), b.len());

        for (k, v) in &pairs {
            let av = a.get(k.as_bytes());
            let bv = b.get(k.as_bytes());
            prop_assert_eq!(av.as_deref(), Some(v.as_bytes()));
            prop_assert_eq!(bv.as_deref(), Some(v.as_bytes()));
        }
        prop_assert!(a.get(b"__definitely_absent__").is_none());
        prop_assert!(b.get(b"__definitely_absent__").is_none());
    }
}
