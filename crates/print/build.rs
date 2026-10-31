//! The engine's version and commit for the slicer's name, read from the workspace lock file so
//! they follow the dependency instead of being kept by hand.

use std::path::Path;

fn main() {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    let text = std::fs::read_to_string(&lock).unwrap_or_default();
    let (version, commit) = sx_core_entry(&text).unwrap_or(("unknown", "unknown"));
    println!("cargo:rustc-env=SX_CORE_VERSION={version}");
    println!("cargo:rustc-env=SX_CORE_COMMIT={commit}");
}

/// The `version` and the short commit of the `sx-core` package in a Cargo.lock text.
fn sx_core_entry(lock: &str) -> Option<(&str, &str)> {
    let mut lines = lock.lines().skip_while(|l| *l != "name = \"sx-core\"");
    lines.next()?;
    let mut version = None;
    let mut commit = None;
    for line in lines.take_while(|l| !l.is_empty()) {
        if let Some(v) = line.strip_prefix("version = ") {
            version = Some(v.trim_matches('"'));
        } else if let Some(s) = line.strip_prefix("source = ") {
            let s = s.trim_matches('"');
            commit = s.rsplit_once('#').map(|(_, c)| c.get(..8).unwrap_or(c));
        }
    }
    Some((version?, commit.unwrap_or("crates.io")))
}
