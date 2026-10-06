//! Rebuild when the web UI build changes. `rust-embed` embeds `ui/dist` at
//! compile time but doesn't tell cargo about it, so without this a binary
//! compiled before `pnpm build` (or before a UI change) would keep serving the
//! old UI — or none. A directory path makes cargo rescan it recursively; a
//! missing one is fine (the UI is optional).
fn main() {
    println!("cargo:rerun-if-changed=../../ui/dist");
}
