use std::path::Path;

fn main() {
    // The frontend is embedded in the binary, so it has to exist before the
    // binary is linked. Without this check a missing or half-built `ui/dist`
    // produces an application that compiles, launches, and shows a blank window
    // or a browser error page — which is exactly how the dev-server footgun
    // used to reach the desktop.
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist/index.html");
    if !dist.exists() {
        panic!(
            "the frontend is not built: {} is missing.\n\
             Build it first:  npm --prefix ui run build",
            dist.display()
        );
    }
    println!("cargo:rerun-if-changed=../../ui/dist");
    tauri_build::build()
}
