//! Proves `recover_read_only` works against a genuinely read-only
//! filesystem, exactly the condition a Docker `:ro` bind mount enforces
//! (the scenario that originally surfaced this: `SegmentedFileStorage::
//! open` unconditionally opens the active segment for writing, which
//! fails under `:ro` even for a caller that only ever calls `recover`).
//!
//! Note: this only checks the positive case (the fix works), not that
//! `open()` still fails the same way, since that check depends on Unix
//! permission bits -- which root bypasses, unlike a real mount-level
//! read-only flag (`EROFS`), so it isn't reliable across environments
//! that may run as root (this sandbox does).

use ledger_log::RecordStorage;
use ledger_store::{SegmentedFileStorage, recover_read_only};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("ledger_store_readonly_test_{label}_{nanos}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn make_read_only(dir: &std::path::Path) {
    // Recursively strip write permission from the directory and
    // everything in it -- mirrors what a `:ro` Docker bind mount enforces
    // at the syscall level, closer to the real failure than just testing
    // against a writable directory and hoping the logic is equivalent.
    fn strip_write(path: &std::path::Path) {
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(perms.mode() & !0o222);
        fs::set_permissions(path, perms).unwrap();
    }
    for entry in walkdir(dir) {
        strip_write(&entry);
    }
    strip_write(dir);
}

fn walkdir(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walkdir(&path));
            }
            out.push(path);
        }
    }
    out
}

#[test]
fn recover_read_only_works_against_a_read_only_filesystem() {
    let dir = temp_dir("basic");

    {
        let mut store = SegmentedFileStorage::open(&dir).unwrap();
        store.append(b"first").unwrap();
        store.append(b"second").unwrap();
        store.sync().unwrap();
    }

    make_read_only(&dir);

    let outcome = recover_read_only(&dir).unwrap();
    assert_eq!(outcome.records, vec![b"first".to_vec(), b"second".to_vec()]);
    assert_eq!(outcome.discarded_tail_bytes, 0);

    // Restore write permission so the OS temp-dir cleanup doesn't fail.
    let mut perms = fs::metadata(&dir).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&dir, perms).unwrap();
    let _ = fs::remove_dir_all(&dir);
}
