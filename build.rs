//! Stamps the binary with the git commit it was built from, so the
//! self-updater can ask GitHub whether `main` has moved past it.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Embed the built web app (`web/dist`, from `bun run build`) as a table of
/// (path, bytes, mime, etag) for `src/web/assets.rs`.
fn embed_web() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("web").join("dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let mut files = Vec::new();
    collect(&dist, &dist, &mut files);
    files.sort();
    let mut out = String::from("pub static ASSETS: &[(&str, &[u8], &str, &str)] = &[\n");
    for rel in &files {
        let abs = dist.join(rel);
        println!("cargo:rerun-if-changed={}", abs.display());
        let bytes = std::fs::read(&abs).unwrap_or_default();
        // FNV-1a: a stable content hash for ETags.
        let etag = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3));
        let ext = rel.rsplit('.').next().unwrap_or("");
        let mime = match ext {
            "html" => "text/html; charset=utf-8",
            "js" | "mjs" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            "json" | "webmanifest" => "application/manifest+json",
            "woff2" => "font/woff2",
            "txt" => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        };
        out.push_str(&format!(
            "    ({rel:?}, include_bytes!({:?}), {mime:?}, \"\\\"{etag:016x}\\\"\"),\n",
            abs.to_string_lossy()
        ));
    }
    if files.is_empty() {
        out.push_str("    (\"index.html\", b\"<!doctype html><meta charset=utf-8><title>blush web</title><body style='background:#1b1b1f;color:#e8e6ea;font:14px monospace;padding:2rem'>The web app isn't built into this binary. Run <code>make web</code> (needs bun), then rebuild blush.\", \"text/html; charset=utf-8\", \"\\\"0\\\"\"),\n");
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("web_assets.rs");
    std::fs::write(dest, out).unwrap();
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(root, &p, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn main() {
    embed_web();
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
