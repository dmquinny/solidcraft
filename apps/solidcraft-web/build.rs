//! The build id the account menu shows (`SOLIDCRAFT_BUILD`): the checkout's git commit, so a
//! browser can say which build it runs (they keep old builds for a while); "dev" without a
//! checkout. An environment variable of that name (CI) wins.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=SOLIDCRAFT_BUILD");
    let git = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.git");
    // Watched whether or not they exist now: when one appears or changes later this must run
    // again, or cargo replays the old answer (a Docker build with a kept target directory did).
    for f in ["HEAD", "packed-refs", "refs/heads"] {
        println!("cargo:rerun-if-changed={}", git.join(f).display());
    }
    let build = std::env::var("SOLIDCRAFT_BUILD").ok().filter(|b| !b.trim().is_empty()).or_else(|| commit(&git)).unwrap_or_else(|| {
        let head = std::fs::read_to_string(git.join("HEAD")).map(|h| h.trim().to_string());
        println!("cargo:warning=SOLIDCRAFT_BUILD: no commit found through {} (HEAD: {head:?}); the build id is \"dev\"", git.display());
        "dev".to_string()
    });
    println!("cargo:rustc-env=SOLIDCRAFT_BUILD={build}");
}

/// The short commit HEAD points at: a hash, or through a branch's ref (loose or packed).
fn commit(git: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let hash = match head.trim().strip_prefix("ref: ") {
        None => head.trim().to_string(),
        Some(r) => match std::fs::read_to_string(git.join(r)) {
            Ok(h) => h.trim().to_string(),
            Err(_) => {
                let packed = std::fs::read_to_string(git.join("packed-refs")).ok()?;
                packed.lines().find_map(|l| l.strip_suffix(r).map(|h| h.trim().to_string()))?
            }
        },
    };
    hash.get(..7).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_string)
}
