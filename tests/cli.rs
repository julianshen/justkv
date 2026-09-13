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
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    let out = bin()
        .arg("check")
        .arg("/nonexistent/justkv/nope.csv")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn build_produces_a_loadable_compiled_file() {
    let src = write("src.csv", b"a,1\nb,2\n");
    let dst = std::env::temp_dir().join(format!("justkv-out-{}.bin", std::process::id()));
    let out = bin()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&dst)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

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
    let out = bin()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&dst)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!dst.exists(), "must not write output when validation fails");
}

#[test]
fn build_preserves_allow_binary_as_a_flag_in_the_file() {
    let src = write("bin.csv", b"k,\xff\xfe\n");
    let dst = std::env::temp_dir().join(format!("justkv-binflag-{}.bin", std::process::id()));
    let out = bin()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&dst)
        .arg("--allow-binary")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let data = std::fs::read(&dst).unwrap();
    let (_, _, flags) = justkv::store::compiled::read_compiled(&data).unwrap();
    assert_eq!(
        flags & justkv::store::compiled::FLAG_BINARY,
        justkv::store::compiled::FLAG_BINARY
    );
}

#[test]
fn build_does_not_disturb_an_unrelated_sibling_tmp_file() {
    // `-o kv.bin` once derived its scratch path by swapping the extension,
    // which truncated whatever `kv.tmp` already held and deleted it on the
    // error paths. That file belongs to the user, not to us.
    let src = write("stem-src.csv", b"a,1\nb,2\n");
    let dir = std::env::temp_dir().join(format!("justkv-stem-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dst = dir.join("kv.bin");
    let bystander = dir.join("kv.tmp");
    std::fs::write(&bystander, b"precious user data").unwrap();

    let out = bin()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&dst)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&bystander).unwrap(),
        b"precious user data",
        "build overwrote an unrelated sibling file"
    );

    // And the scratch file itself must not be left behind.
    let strays: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n != "kv.bin" && n != "kv.tmp")
        .collect();
    assert!(strays.is_empty(), "left scratch files behind: {strays:?}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn build_failure_leaves_no_scratch_file_behind() {
    let src = write("stem-bad.csv", b"a,1\na,2\n");
    let dir = std::env::temp_dir().join(format!("justkv-stemfail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dst = dir.join("kv.bin");

    let out = bin()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&dst)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    assert!(left.is_empty(), "left files behind after failure: {left:?}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn healthcheck_fails_when_nothing_is_listening() {
    let out = bin()
        .arg("healthcheck")
        .arg("--bind")
        .arg("127.0.0.1:1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}
