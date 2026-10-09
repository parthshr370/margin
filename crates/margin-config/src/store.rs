//! Reading and writing sidecar files (CU-SC-1, CU-SC-3): `<file>.margin.json` beside the
//! document, or `<data>/sidecars/<fingerprint>.margin.json` when the document's folder cannot be
//! written. This is the only code that touches sidecar files; callers hand it a [`Sidecar`].
//!
//! Not here yet: `sidecars/index.json` and `book_id` matching for moved files (CU-SC-6). A
//! document's sidecar is found by its path first and by its fingerprint second.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use md5::{Digest as _, Md5};

use crate::sidecar::{Sidecar, SidecarFormat};

/// The suffix of every sidecar file.
pub const SIDECAR_SUFFIX: &str = ".margin.json";

/// The sidecar format version this build writes. A newer file opens read-only (CU-VAL-4).
pub const SIDECAR_VERSION: u32 = 1;

/// Environment variable that overrides `<data>`, for tests and portable setups.
pub const DATA_DIR_ENV: &str = "MARGIN_DATA_DIR";

/// The folder of central sidecars inside `<data>`.
const CENTRAL_DIR: &str = "sidecars";

/// Largest sidecar read, in bytes. Reading state is small; a bigger file is not one of ours.
pub const MAX_SIDECAR_BYTES: u64 = 16 << 20;

/// KOReader's `partialMD5`: 1 KiB samples at offset 0 and at 1024·4^i for i = 0..=10.
const FINGERPRINT_SAMPLE: usize = 1024;
const FINGERPRINT_STEPS: u32 = 10;

/// `<data>`: `$MARGIN_DATA_DIR` if set, else the OS data dir plus `margin`
/// (`~/.local/share/margin` on Linux). `None` when the OS reports no data dir.
pub fn data_dir() -> Option<PathBuf> {
    match std::env::var_os(DATA_DIR_ENV) {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::data_dir().map(|dir| dir.join("margin")),
    }
}

/// The sidecar fingerprint of a file's bytes (CU-SC-2): KOReader's `util.partialMD5`, lower-case
/// hex. LuaJIT's `lshift(1024, -2)` is 0, so the first sample is the file's start.
pub fn fingerprint(bytes: &[u8]) -> String {
    let mut hash = Md5::new();
    let offsets = std::iter::once(0usize).chain((0..=FINGERPRINT_STEPS).map(|i| FINGERPRINT_SAMPLE << (2 * i)));
    for offset in offsets {
        let Some(sample) = bytes.get(offset..) else { break };
        if sample.is_empty() {
            break;
        }
        hash.update(&sample[..sample.len().min(FINGERPRINT_SAMPLE)]);
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `<file>.margin.json` next to `document`.
pub fn beside_path(document: &Path) -> Option<PathBuf> {
    let name = document.file_name()?;
    let mut sidecar = name.to_os_string();
    sidecar.push(SIDECAR_SUFFIX);
    Some(document.with_file_name(sidecar))
}

/// `<data>/sidecars/<fingerprint>.margin.json`.
pub fn central_path(data: &Path, fingerprint: &str) -> PathBuf {
    data.join(CENTRAL_DIR).join(format!("{fingerprint}{SIDECAR_SUFFIX}"))
}

/// What [`find`] found for a document.
#[derive(Debug)]
pub enum Found {
    /// No sidecar yet.
    Missing,
    /// A sidecar Margin can read and rewrite.
    Sidecar {
        /// Its contents.
        sidecar: Box<Sidecar>,
        /// Where it is.
        path: PathBuf,
    },
    /// A sidecar Margin must not overwrite: written by a newer Margin, or unreadable.
    Locked {
        /// Where it is.
        path: PathBuf,
        /// Why it is left alone.
        reason: String,
    },
}

/// Looks for `document`'s sidecar: beside it first, then in `<data>` by fingerprint.
pub fn find(document: &Path, fingerprint: &str, data: Option<&Path>) -> Found {
    let candidates = beside_path(document).into_iter().chain(data.map(|data| central_path(data, fingerprint)));
    for path in candidates {
        match read(&path) {
            Ok(None) => continue,
            Ok(Some(sidecar)) if sidecar.version > SIDECAR_VERSION => {
                let reason = format!("written by a newer Margin (version {}); it is not changed", sidecar.version);
                return Found::Locked { path, reason };
            }
            Ok(Some(sidecar)) => return Found::Sidecar { sidecar: Box::new(sidecar), path },
            Err(reason) => return Found::Locked { path, reason: format!("{reason}; it is not changed") },
        }
    }
    Found::Missing
}

/// Reads one sidecar file. `Ok(None)` when it does not exist.
pub fn read(path: &Path) -> Result<Option<Sidecar>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {} ({err})", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SIDECAR_BYTES.saturating_add(1)).read_to_end(&mut bytes).map_err(|err| format!("cannot read {} ({err})", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SIDECAR_BYTES {
        return Err(format!("{} is larger than {} MiB", path.display(), MAX_SIDECAR_BYTES >> 20));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|err| format!("{} is not a Margin sidecar ({err})", path.display()))
}

/// Writes `sidecar` for `document`: to `preferred` when given (where it was found), else beside
/// the document; when that folder cannot be written, to `<data>/sidecars/<fingerprint>…`.
/// Bumps `revision` and sets `modified`. Returns where it went.
pub fn write(sidecar: &mut Sidecar, document: &Path, fingerprint: &str, data: Option<&Path>, preferred: Option<&Path>) -> Result<PathBuf, String> {
    sidecar.revision = sidecar.revision.saturating_add(1);
    sidecar.modified = Some(now_rfc3339());
    let bytes = serde_json::to_vec_pretty(sidecar).map_err(|err| format!("cannot encode the sidecar ({err})"))?;
    let first = preferred.map(Path::to_path_buf).or_else(|| beside_path(document)).ok_or("the document has no file name")?;
    let first_error = match write_atomically(&first, &bytes) {
        Ok(()) => return Ok(first),
        Err(err) => format!("{} ({err})", first.display()),
    };
    let central = data.map(|data| central_path(data, fingerprint)).filter(|central| *central != first).ok_or(first_error.clone())?;
    if let Some(dir) = central.parent() {
        std::fs::create_dir_all(dir).map_err(|err| format!("{first_error}; {} ({err})", dir.display()))?;
    }
    write_atomically(&central, &bytes).map_err(|err| format!("{first_error}; {} ({err})", central.display()))?;
    Ok(central)
}

/// Write via a temporary file in the same folder, then rename it over `path`.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static STAGING: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let staging = dir.join(format!(".{name}.{}-{}.tmp", std::process::id(), STAGING.fetch_add(1, Ordering::Relaxed)));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&staging)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    let renamed = written.and_then(|()| std::fs::rename(&staging, path));
    if renamed.is_err()
        && let Err(err) = std::fs::remove_file(&staging)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("sidecar: could not remove the staging file {} ({err})", staging.display());
    }
    renamed
}

impl Sidecar {
    /// An empty version-1 sidecar.
    pub fn new() -> Self {
        Self {
            format: SidecarFormat::MarginSidecar,
            version: SIDECAR_VERSION,
            revision: 0,
            modified: None,
            deleted: Vec::new(),
            book_id: None,
            migrated_to: None,
            doc: None,
            progress: None,
            settings: None,
            highlights: Vec::new(),
            bookmarks: Vec::new(),
            notes: Vec::new(),
            trim: None,
            obsidian: None,
            extra: serde_json::Map::new(),
        }
    }
}

impl Default for Sidecar {
    fn default() -> Self {
        Self::new()
    }
}

/// Crockford's base 32, as ULIDs use it.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// ULID: 48 bits of milliseconds, then 80 random bits; 26 characters of 5 bits.
const ULID_CHARS: u32 = 26;
const ULID_BITS_PER_CHAR: u32 = 5;
const ULID_RANDOM_BITS: u32 = 80;

/// A new ULID (CU-SC-3 ids). The random part comes from the standard library's per-process
/// random hash keys, mixed with a counter: unique within and across runs, not cryptographic.
pub fn new_ulid() -> String {
    use std::hash::{BuildHasher as _, Hasher as _};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let random_word = |salt: u64| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(millis);
        hasher.write_u64(count);
        hasher.write_u64(salt);
        hasher.write_u32(std::process::id());
        hasher.finish()
    };
    let random = (u128::from(random_word(0)) << 64 | u128::from(random_word(1))) & ((1u128 << ULID_RANDOM_BITS) - 1);
    let value = (millis & ((1u128 << 48) - 1)) << ULID_RANDOM_BITS | random;
    (0..ULID_CHARS)
        .rev()
        .map(|i| {
            let index = (value >> (i * ULID_BITS_PER_CHAR)) & 0x1f;
            char::from(CROCKFORD[index as usize])
        })
        .collect()
}

/// Now as RFC 3339 UTC to the second (`2026-10-09T18:11:56Z`).
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    rfc3339(secs)
}

const SECS_PER_DAY: u64 = 86_400;

/// Seconds since the Unix epoch as RFC 3339 UTC (Howard Hinnant's `civil_from_days`).
fn rfc3339(secs: u64) -> String {
    let (days, rest) = (secs / SECS_PER_DAY, secs % SECS_PER_DAY);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("margin-store-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fingerprints_sample_like_koreader() {
        // Offsets 0, 1024, 4096, 16384…: a 5000-byte file hashes 0..1024, 1024..2048, 4096..5000.
        let bytes: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mut expected = Md5::new();
        expected.update(&bytes[..1024]);
        expected.update(&bytes[1024..2048]);
        expected.update(&bytes[4096..]);
        let hex: String = expected.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(fingerprint(&bytes), hex);
        assert_eq!(fingerprint(b"").len(), 32);
    }

    #[test]
    fn rfc3339_dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_791_569_516), "2026-10-09T18:11:56Z");
    }

    #[test]
    fn ulids_are_26_crockford_characters_and_unique() {
        let a = new_ulid();
        let b = new_ulid();
        assert_eq!(a.len(), 26);
        assert!(a.bytes().all(|c| CROCKFORD.contains(&c)));
        assert_ne!(a, b);
    }

    #[test]
    fn writes_beside_then_finds_it_and_keeps_unknown_fields() {
        let dir = temp_dir("beside");
        let doc = dir.join("paper.pdf");
        std::fs::write(&doc, b"%PDF-1.7").unwrap();
        let mut sidecar = Sidecar::new();
        sidecar.extra.insert("future".into(), serde_json::json!({ "kept": true }));
        let path = write(&mut sidecar, &doc, "f00", None, None).unwrap();
        assert_eq!(path, dir.join("paper.pdf.margin.json"));
        let Found::Sidecar { sidecar: back, path: found } = find(&doc, "f00", None) else { panic!("not found") };
        assert_eq!(found, path);
        assert_eq!(back.revision, 1);
        assert_eq!(back.extra["future"]["kept"], true);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn newer_and_broken_sidecars_are_locked() {
        let dir = temp_dir("locked");
        let doc = dir.join("a.pdf");
        let beside = beside_path(&doc).unwrap();
        std::fs::write(&beside, r#"{"format":"margin-sidecar","version":9}"#).unwrap();
        assert!(matches!(find(&doc, "x", None), Found::Locked { .. }));
        std::fs::write(&beside, "{ not json").unwrap();
        assert!(matches!(find(&doc, "x", None), Found::Locked { .. }));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder Margin cannot write sends the sidecar to `<data>/sidecars/<fingerprint>…`, and
    /// the next open finds it there.
    #[cfg(unix)]
    #[test]
    fn unwritable_folders_fall_back_to_the_data_dir() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("readonly");
        let docs = dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let doc = docs.join("book.pdf");
        std::fs::write(&doc, b"%PDF").unwrap();
        std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o555)).unwrap();
        let data = dir.join("data");
        // Root ignores permissions: nothing to test there.
        let probe = docs.join(".probe");
        if std::fs::write(&probe, b"").is_ok() {
            std::fs::remove_file(&probe).unwrap();
        } else {
            let path = write(&mut Sidecar::new(), &doc, "abc", Some(&data), None).unwrap();
            assert_eq!(path, data.join("sidecars").join("abc.margin.json"));
            assert!(matches!(find(&doc, "abc", Some(&data)), Found::Sidecar { .. }));
        }
        std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
