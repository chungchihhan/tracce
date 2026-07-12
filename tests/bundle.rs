use std::path::Path;
use tempfile::TempDir;
use tracce::trace::session::Meta;
use tracce::view::discovery::entry_for_dir;

/// Write a minimal valid session dir under `root/sessions/<id>/` and return its path.
fn make_session(root: &Path, id: &str) -> std::path::PathBuf {
    let d = root.join("sessions").join(id);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("events.jsonl"), "{\"a\":1}\n{\"b\":2}\n").unwrap();
    std::fs::write(d.join("status"), "done\n").unwrap();
    std::fs::write(
        d.join("meta.json"),
        format!(
            r#"{{
        "session_id": "{id}",
        "started_at": "2026-05-28T22:04:31Z",
        "ended_at": null,
        "cwd": "/tmp/demo",
        "argv": ["claude"],
        "claude_pid": 4711,
        "tracer_pid": 4710,
        "hostname": "h",
        "macos_version": "15.4",
        "tracce_version": "0.1.0"
    }}"#
        ),
    )
    .unwrap();
    d
}

/// Build a raw .tar.gz from (name, bytes) pairs — bypasses `export` so tests can
/// craft malicious archives. Entry names are written directly into the GNU
/// header so traversal paths like `../evil` survive (the tar builder's own
/// `set_path` would otherwise reject them, which is what we want to test against
/// on the *read* side).
fn make_targz(out: &Path, entries: &[(&str, &[u8])]) {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let f = std::fs::File::create(out).unwrap();
    let gz = GzEncoder::new(f, Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (name, data) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        // Write the raw name bytes directly, skipping set_path's traversal guard.
        let name_bytes = name.as_bytes();
        let field = &mut h.as_gnu_mut().unwrap().name;
        field.iter_mut().for_each(|b| *b = 0);
        field[..name_bytes.len()].copy_from_slice(name_bytes);
        h.set_cksum();
        tar.append(&h, *data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
}

fn good_meta(id: &str) -> String {
    format!(
        r#"{{
        "session_id": "{id}",
        "started_at": "2026-05-28T22:04:31Z",
        "ended_at": null,
        "cwd": "/tmp/demo",
        "argv": ["claude"],
        "claude_pid": 4711,
        "tracer_pid": 4710,
        "hostname": "h",
        "macos_version": "15.4",
        "tracce_version": "0.1.0"
    }}"#
    )
}

#[test]
fn export_writes_nonempty_targz() {
    let root = TempDir::new().unwrap();
    let dir = make_session(root.path(), "2026-05-28T22-04-31_demo_4711");
    let entry = entry_for_dir(&dir).unwrap();
    let out = root.path().join("bundle.tracce.tgz");
    let n = tracce::bundle::export(&entry, &out).unwrap();
    assert!(n > 0, "export should report bytes written");
    assert!(out.exists());
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "should be gzip");
}

#[test]
fn import_roundtrip_restores_files() {
    let src_root = TempDir::new().unwrap();
    let id = "2026-05-28T22-04-31_demo_4711";
    let dir = make_session(src_root.path(), id);
    let entry = entry_for_dir(&dir).unwrap();
    let out = src_root.path().join("b.tgz");
    tracce::bundle::export(&entry, &out).unwrap();

    let dst_root = TempDir::new().unwrap();
    let got = tracce::bundle::import(&out, dst_root.path(), false).unwrap();
    assert_eq!(got, id);
    let d = dst_root.path().join("sessions").join(id);
    assert_eq!(
        std::fs::read(d.join("events.jsonl")).unwrap(),
        std::fs::read(dir.join("events.jsonl")).unwrap()
    );
    assert_eq!(
        std::fs::read(d.join("status")).unwrap(),
        std::fs::read(dir.join("status")).unwrap()
    );
    let m: Meta = serde_json::from_slice(&std::fs::read(d.join("meta.json")).unwrap()).unwrap();
    assert_eq!(m.session_id, id);
}

#[test]
fn import_rejects_path_traversal_entry() {
    let root = TempDir::new().unwrap();
    let out = root.path().join("evil.tgz");
    make_targz(
        &out,
        &[
            ("../evil", b"pwned"),
            ("meta.json", good_meta("x").as_bytes()),
            ("status", b"done\n"),
            ("events.jsonl", b"{}\n"),
        ],
    );
    let dst = TempDir::new().unwrap();
    let err = tracce::bundle::import(&out, dst.path(), false);
    assert!(err.is_err(), "traversal entry must be rejected");
    assert!(!dst.path().join("evil").exists());
    assert!(!root.path().join("evil").exists());
}

#[test]
fn import_rejects_oversized_entry() {
    let root = TempDir::new().unwrap();
    let out = root.path().join("big.tgz");
    // status has a tiny cap; 10 MB into it must be rejected.
    let huge = vec![b'x'; 10 * 1024 * 1024];
    make_targz(
        &out,
        &[
            ("events.jsonl", b"{}\n"),
            ("meta.json", good_meta("x").as_bytes()),
            ("status", &huge),
        ],
    );
    let dst = TempDir::new().unwrap();
    assert!(tracce::bundle::import(&out, dst.path(), false).is_err());
}

#[test]
fn import_rejects_malicious_session_id() {
    let root = TempDir::new().unwrap();
    let out = root.path().join("m.tgz");
    make_targz(
        &out,
        &[
            ("events.jsonl", b"{}\n"),
            ("meta.json", good_meta("../../escape").as_bytes()),
            ("status", b"done\n"),
        ],
    );
    let dst = TempDir::new().unwrap();
    assert!(tracce::bundle::import(&out, dst.path(), false).is_err());
    assert!(!dst.path().parent().unwrap().join("escape").exists());
}

#[test]
fn import_refuses_to_clobber_without_force() {
    let src_root = TempDir::new().unwrap();
    let id = "2026-05-28T22-04-31_demo_4711";
    let dir = make_session(src_root.path(), id);
    let entry = entry_for_dir(&dir).unwrap();
    let out = src_root.path().join("b.tgz");
    tracce::bundle::export(&entry, &out).unwrap();

    let dst = TempDir::new().unwrap();
    tracce::bundle::import(&out, dst.path(), false).unwrap();
    // Second import without force fails; with force succeeds.
    assert!(tracce::bundle::import(&out, dst.path(), false).is_err());
    assert_eq!(tracce::bundle::import(&out, dst.path(), true).unwrap(), id);
}

#[test]
fn import_rejects_duplicate_entry() {
    // A second meta.json (with a different id) must not silently redirect the
    // destination — the import must be rejected outright.
    let root = TempDir::new().unwrap();
    let out = root.path().join("dup.tgz");
    make_targz(
        &out,
        &[
            ("events.jsonl", b"{}\n"),
            ("meta.json", good_meta("2026-05-28T22-04-31_first_1").as_bytes()),
            ("status", b"done\n"),
            ("meta.json", good_meta("2026-05-28T22-04-31_second_2").as_bytes()),
        ],
    );
    let dst = TempDir::new().unwrap();
    let err = tracce::bundle::import(&out, dst.path(), false);
    assert!(err.is_err(), "duplicate entry must be rejected");
    // Neither id should have been created.
    assert!(!dst
        .path()
        .join("sessions")
        .join("2026-05-28T22-04-31_second_2")
        .exists());
    assert!(!dst
        .path()
        .join("sessions")
        .join("2026-05-28T22-04-31_first_1")
        .exists());
}

#[test]
fn import_accepts_entry_exactly_at_cap() {
    // status cap is 64 KiB; an entry of exactly that size must be accepted.
    let root = TempDir::new().unwrap();
    let out = root.path().join("atcap.tgz");
    let exact = vec![b'd'; 64 * 1024];
    make_targz(
        &out,
        &[
            ("events.jsonl", b"{}\n"),
            ("meta.json", good_meta("2026-05-28T22-04-31_cap_1").as_bytes()),
            ("status", &exact),
        ],
    );
    let dst = TempDir::new().unwrap();
    assert_eq!(
        tracce::bundle::import(&out, dst.path(), false).unwrap(),
        "2026-05-28T22-04-31_cap_1"
    );
}

#[test]
fn import_accepts_pre_rename_ctrace_version() {
    let root = TempDir::new().unwrap();
    let out = root.path().join("old.tgz");
    let id = "2026-05-28T10-00-00_old_1";
    let meta = format!(
        r#"{{
        "session_id": "{id}",
        "started_at": "2026-05-28T10:00:00Z",
        "ended_at": null,
        "cwd": "/tmp/old",
        "argv": ["claude"],
        "claude_pid": 1,
        "tracer_pid": 1,
        "hostname": "h",
        "macos_version": "15.4",
        "ctrace_version": "0.1.0"
    }}"#
    );
    make_targz(
        &out,
        &[
            ("events.jsonl", b"{}\n"),
            ("meta.json", meta.as_bytes()),
            ("status", b"done\n"),
        ],
    );
    let dst = TempDir::new().unwrap();
    assert_eq!(tracce::bundle::import(&out, dst.path(), false).unwrap(), id);
}

#[test]
fn cli_export_then_import_roundtrip() {
    use std::process::Command;
    fn bin() -> Command {
        Command::new(env!("CARGO_BIN_EXE_tracce"))
    }

    let src = TempDir::new().unwrap();
    let id = "2026-05-28T22-04-31_demo_4711";
    make_session(src.path(), id);
    let out = src.path().join("s.tracce.tgz");

    let e = bin()
        .env("TRACCE_HOME", src.path())
        .args(["export", id, "-o"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        e.status.success(),
        "export stderr: {}",
        String::from_utf8_lossy(&e.stderr)
    );
    assert!(out.exists());

    let dst = TempDir::new().unwrap();
    let i = bin()
        .env("TRACCE_HOME", dst.path())
        .arg("import")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        i.status.success(),
        "import stderr: {}",
        String::from_utf8_lossy(&i.stderr)
    );
    assert!(dst
        .path()
        .join("sessions")
        .join(id)
        .join("events.jsonl")
        .exists());

    // Imported session shows up in `list`.
    let l = bin()
        .env("TRACCE_HOME", dst.path())
        .arg("list")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&l.stdout).contains("demo"));
}
