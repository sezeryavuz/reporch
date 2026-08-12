//! `rp` — the short alias, installed alongside `reporch` by every channel (§14).
//!
//! A real second binary rather than a symlink, so it behaves identically however reporch was
//! installed: `cargo install`, Homebrew, the shell installer, and the npm shim.

fn main() -> std::process::ExitCode {
    reporch::run()
}
