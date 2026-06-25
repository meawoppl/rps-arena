//! Guarantee the embedded-asset directory exists before compilation.
//!
//! `embedded_assets.rs` derives `RustEmbed` over `../frontend/dist`, which is a
//! gitignored build artifact produced by `trunk build` in `frontend/`. In a
//! clean checkout that directory is absent, and the derive then fails to
//! compile the backend — including `cargo test -p backend` and `cargo check`,
//! neither of which needs a real frontend.
//!
//! This build script runs before the crate (and its proc-macros) are expanded,
//! so creating a placeholder here is enough to let the backend build on its
//! own. A real build runs `trunk build` first and populates `dist` with the
//! compiled WASM bundle; we only write the placeholder when nothing is there,
//! so a genuine frontend is never clobbered.

use std::path::Path;

fn main() {
    let dist = Path::new("../frontend/dist");
    if !dist.exists() {
        std::fs::create_dir_all(dist).expect("create ../frontend/dist placeholder directory");
    }

    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::write(
            &index,
            "<!doctype html>\n<meta charset=\"utf-8\">\n<title>RPS Arena</title>\n\
             <p>Frontend not built. Run <code>trunk build</code> in <code>frontend/</code>.</p>\n",
        )
        .expect("write ../frontend/dist/index.html placeholder");
    }

    // Emit the short git SHA so /api/health can report exactly what this binary
    // was built from. Falls back to "unknown" outside a git checkout (e.g. a
    // source tarball).
    let git_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_SHA={git_sha}");

    // Rebuild when HEAD moves so the reported SHA stays accurate. (Guarded so a
    // missing .git doesn't force an unconditional rebuild every time.)
    if Path::new("../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../.git/HEAD");
    }
}
