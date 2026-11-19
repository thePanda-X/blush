//! Stamps the binary with the git commit it was built from, so the
//! self-updater can ask GitHub whether `main` has moved past it.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_default();
    let dirty = !sha.is_empty()
        && git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());

    let version = env!("CARGO_PKG_VERSION");
    let long = match (sha.get(..7), dirty) {
        (Some(short), false) => format!("{version} ({short})"),
        (Some(short), true) => format!("{version} ({short}-dirty)"),
        (None, _) => version.to_string(),
    };
    println!("cargo:rustc-env=BLUSH_GIT_SHA={sha}");
    println!("cargo:rustc-env=BLUSH_GIT_DIRTY={}", if dirty { "1" } else { "0" });
    println!("cargo:rustc-env=BLUSH_VERSION={long}");

    // Re-stamp when the checked-out commit or the working tree changes.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        let git_dir = Path::new(&git_dir);
        for f in ["HEAD", "index", "packed-refs"] {
            println!("cargo:rerun-if-changed={}", git_dir.join(f).display());
        }
        if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
            && let Some(r) = head.trim().strip_prefix("ref: ")
        {
            println!("cargo:rerun-if-changed={}", git_dir.join(r).display());
        }
    }
}
