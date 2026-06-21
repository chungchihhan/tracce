//! Export / import a tracce session as a single gzip tarball (`.tracce.tgz`).
//!
//! A session is a directory of exactly three files. Export bundles them at the
//! archive root; import extracts them back under `<root>/sessions/<id>/`.
//! Import is the only place tracce ingests an untrusted file, so its extraction
//! is hardened deliberately (see `import`).

use crate::trace::session::Meta;
use crate::view::discovery::SessionEntry;
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// The exact files that make up a session. Import accepts only these names.
pub const SESSION_FILES: [&str; 3] = ["events.jsonl", "meta.json", "status"];

/// Per-entry decompression ceilings. `events.jsonl` gets the full cap; meta and
/// status are clamped far smaller so a bomb in a small file is caught early.
pub const MAX_ENTRY_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB for events.jsonl
const MAX_META_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB
const MAX_STATUS_BYTES: u64 = 64 * 1024; // 64 KiB

fn cap_for(name: &str) -> u64 {
    match name {
        "meta.json" => MAX_META_BYTES,
        "status" => MAX_STATUS_BYTES,
        _ => MAX_ENTRY_BYTES,
    }
}

/// Bundle `entry`'s session directory into a gzip tarball at `out`.
/// Returns the number of bytes written. Entries are stored at the archive root
/// with normalized metadata (mode 0644, zeroed uid/gid/mtime) for reproducible,
/// host-independent archives.
pub fn export(entry: &SessionEntry, out: &Path) -> Result<u64> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let file = File::create(out).with_context(|| format!("create {}", out.display()))?;
    let gz = GzEncoder::new(file, Compression::default());
    let mut tar = tar::Builder::new(gz);

    for name in SESSION_FILES {
        let src = entry.dir.join(name);
        let data = std::fs::read(&src)
            .with_context(|| format!("read session file {}", src.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("append {name} to archive"))?;
    }

    let gz = tar.into_inner().context("finish tar")?;
    let mut file = gz.finish().context("finish gzip")?;
    file.flush().ok();
    let n = file
        .metadata()
        .map(|m| m.len())
        .or_else(|_| std::fs::metadata(out).map(|m| m.len()))
        .unwrap_or(0);
    if n == 0 {
        bail!("export produced an empty archive");
    }
    Ok(n)
}

/// Assert `id` is a single safe path component (no traversal, no separators).
fn validate_session_id(id: &str) -> Result<()> {
    if id.is_empty() || id.contains('\0') {
        bail!("invalid session id");
    }
    let mut comps = Path::new(id).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(c)), None) if c == std::ffi::OsStr::new(id) => Ok(()),
        _ => bail!("session id `{id}` is not a single path component"),
    }
}

/// Validate one archive entry path and return its bare allowlisted filename.
/// Rejects absolute paths, any `..`/non-`Normal` component, and any name not in
/// `SESSION_FILES`.
fn safe_entry_name(path: &Path) -> Result<String> {
    let mut comps = path.components();
    let only = match (comps.next(), comps.next()) {
        (Some(Component::Normal(c)), None) => c,
        _ => bail!("unsafe archive entry path: {}", path.display()),
    };
    let name = only.to_str().context("non-utf8 entry name")?;
    if !SESSION_FILES.contains(&name) {
        bail!("archive entry `{name}` is not an allowed session file");
    }
    Ok(name.to_string())
}

/// Extract `archive` into `<root>/sessions/<id>/`. Returns the session id.
///
/// Hardening: only the three allowlisted filenames are accepted (any other,
/// absolute, or `..`-containing entry, or any non-regular-file entry, is
/// rejected); each entry is read through a byte cap; the destination id (from
/// meta.json) is validated as a single path component; extraction happens in a
/// temp dir and is renamed into place atomically, never clobbering an existing
/// session unless `force`.
pub fn import(archive: &Path, root: &Path, force: bool) -> Result<String> {
    use flate2::read::GzDecoder;

    let file = File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let gz = GzDecoder::new(file);
    let mut ar = tar::Archive::new(gz);
    ar.set_preserve_permissions(false);
    ar.set_unpack_xattrs(false);

    // Stage into a temp dir under root so the final rename is atomic and on the
    // same filesystem.
    std::fs::create_dir_all(root).with_context(|| format!("create {}", root.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".import-")
        .tempdir_in(root)
        .context("create staging dir")?;

    let mut seen: Vec<String> = Vec::new();
    for entry in ar.entries().context("read archive entries")? {
        let mut entry = entry.context("read archive entry")?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            bail!("archive contains a non-regular-file entry");
        }
        let name = {
            let path = entry.path().context("entry path")?;
            safe_entry_name(&path)?
        };
        let cap = cap_for(&name);
        let dest = staging.path().join(&name);
        let mut out = File::create(&dest).with_context(|| format!("write {name}"))?;
        let mut limited = entry.by_ref().take(cap + 1);
        let written =
            std::io::copy(&mut limited, &mut out).with_context(|| format!("extract {name}"))?;
        if written > cap {
            bail!("entry `{name}` exceeds the {cap}-byte limit");
        }
        seen.push(name);
    }

    // All three files must be present.
    for required in SESSION_FILES {
        if !seen.iter().any(|s| s == required) {
            bail!("archive is missing `{required}`");
        }
    }

    // Validate meta.json and derive the destination id.
    let meta_bytes = std::fs::read(staging.path().join("meta.json"))?;
    let meta: Meta =
        serde_json::from_slice(&meta_bytes).context("meta.json is not a valid tracce session")?;
    validate_session_id(&meta.session_id)?;

    let dest_dir: PathBuf = root.join("sessions").join(&meta.session_id);
    if dest_dir.exists() {
        if !force {
            bail!(
                "session `{}` already exists (use --force to overwrite)",
                meta.session_id
            );
        }
        std::fs::remove_dir_all(&dest_dir)
            .with_context(|| format!("remove existing {}", dest_dir.display()))?;
    }
    if let Some(parent) = dest_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Move the validated files into place. `staging` (a TempDir guard) stays
    // alive and cleans up the now-empty staging dir when it drops at the end of
    // the function — so we move per file rather than consuming the guard, which
    // avoids depending on TempDir::keep / the deprecated into_path across the
    // version range the MSRV-pinned lockfile may resolve.
    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("create {}", dest_dir.display()))?;
    for name in SESSION_FILES {
        let from = staging.path().join(name);
        let to = dest_dir.join(name);
        // rename is atomic within a filesystem; fall back to copy across devices.
        if std::fs::rename(&from, &to).is_err() {
            std::fs::copy(&from, &to).with_context(|| format!("install {name}"))?;
        }
    }

    Ok(meta.session_id)
}
