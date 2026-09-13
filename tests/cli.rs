use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_justkv"))
}

fn write(name: &str, contents: &[u8]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("justkv-test-{}-{}", std::process::id(), name));
    std::fs::write(&p, contents).unwrap();
    p
}

#[test]
fn check_exits_zero_on_clean_data() {
    let p = write("clean.csv", b"a,1\nb,2\n");
    let out = bin().arg("check").arg(&p).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn check_exits_one_and_lists_every_problem() {
    let p = write("dirty.csv", b"a,1\nb\nc,2,3\na,9\n");
    let out = bin().arg("check").arg(&p).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("line 3"), "{err}");
    assert!(err.contains("line 4"), "{err}");
    assert!(err.contains("duplicate key"), "{err}");
}

#[test]
fn check_exits_two_on_missing_file() {
    let out = bin().arg("check").arg("/nonexistent/justkv/nope.csv").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn build_produces_a_loadable_compiled_file() {
    let src = write("src.csv", b"a,1\nb,2\n");
    let dst = std::env::temp_dir().join(format!("justkv-out-{}.bin", std::process::id()));
    let out = bin().arg("build").arg(&src).arg("-o").arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    let data = std::fs::read(&dst).unwrap();
    assert!(justkv::store::compiled::is_compiled(&data));
    let loaded = justkv::store::load(&dst, &Default::default()).unwrap();
    assert_eq!(loaded.format, justkv::store::SourceFormat::Compiled);
    assert_eq!(loaded.store.get(b"a").as_deref(), Some(&b"1"[..]));
}

#[test]
fn build_refuses_invalid_data() {
    let src = write("bad.csv", b"a,1\na,2\n");
    let dst = std::env::temp_dir().join(format!("justkv-bad-{}.bin", std::process::id()));
    let out = bin().arg("build").arg(&src).arg("-o").arg(&dst).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!dst.exists(), "must not write output when validation fails");
}

#[test]
fn build_preserves_allow_binary_as_a_flag_in_the_file() {
    let src = write("bin.csv", b"k,\xff\xfe\n");
    let dst = std::env::temp_dir().join(format!("justkv-binflag-{}.bin", std::process::id()));
    let out = bin().arg("build").arg(&src).arg("-o").arg(&dst)
        .arg("--allow-binary").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let data = std::fs::read(&dst).unwrap();
    let (_, _, flags) = justkv::store::compiled::read_compiled(&data).unwrap();
    assert_eq!(flags & justkv::store::compiled::FLAG_BINARY, justkv::store::compiled::FLAG_BINARY);
}
