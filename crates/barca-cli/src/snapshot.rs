//! Checked-in files a test compares its output with, or rewrites with
//! `BARCA_UPDATE_SNAPSHOTS=1` (`scripts/update-cli-snapshots.sh`): the help snapshots and the
//! generated blocks of `contract.rs`, and the site pages of `site_pages_tests.rs`.

use std::path::{Path, PathBuf};

pub(crate) fn updating() -> bool {
    std::env::var("BARCA_UPDATE_SNAPSHOTS").is_ok_and(|v| !v.is_empty() && v != "0")
}

pub(crate) fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(crate) const UPDATE_HINT: &str = "If this change to the CLI surface is deliberate, update the snapshots \
     and the contract in the same PR: scripts/update-cli-snapshots.sh (or \
     BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca), then review the diff. Pre-1.0 a breaking \
     change needs a minor bump and a \"Breaking\" line in the release notes (barca docs contract).";

/// Compare `actual` with the file at `path`, or write it when updating.
pub(crate) fn check_file(path: &Path, actual: &str) {
    if updating() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(path).unwrap_or_else(|_| {
        panic!("missing snapshot {}\n\n{UPDATE_HINT}", path.display());
    });
    if expected != actual {
        panic!(
            "{} is out of date.\n\n{}\n\n{UPDATE_HINT}",
            path.display(),
            first_difference(&expected, actual)
        );
    }
}

pub(crate) fn first_difference(expected: &str, actual: &str) -> String {
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    for i in 0..e.len().max(a.len()) {
        let (el, al) = (e.get(i).copied(), a.get(i).copied());
        if el != al {
            return format!(
                "first difference at line {}:\n  snapshot: {}\n  now:      {}",
                i + 1,
                el.unwrap_or("<end of file>"),
                al.unwrap_or("<end of file>")
            );
        }
    }
    "the files differ only in trailing whitespace or line endings".to_string()
}
