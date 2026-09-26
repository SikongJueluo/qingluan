//! Re-embed the web bundle when it changes.
//!
//! `include_dir!` reads files during macro expansion, which rustc does not
//! by itself re-run when only the embedded directory content changes.
fn main() {
    println!("cargo:rerun-if-changed=../../apps/web/dist");
}
