// Compile the vendored apple_cloud_notes_parser schema with protox (pure Rust, no protoc).
fn main() {
    let dir = "vendor/apple_cloud_notes_parser";
    println!("cargo:rerun-if-changed={dir}/notestore.proto");
    let fds = protox::compile(["notestore.proto"], [dir]).expect("parse notestore.proto");
    prost_build::Config::new()
        .compile_fds(fds)
        .expect("generate rust from notestore.proto");
}
