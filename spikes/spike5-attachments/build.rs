// Tauri context for the `fetch` bin, plus the vendored apple_cloud_notes_parser schema
// compiled with protox (pure Rust, no protoc), as in spike 3.
fn main() {
    tauri_build::build();
    let dir = "vendor/apple_cloud_notes_parser";
    println!("cargo:rerun-if-changed={dir}/notestore.proto");
    let fds = protox::compile(["notestore.proto"], [dir]).expect("parse notestore.proto");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("generate rust from notestore.proto");
}
