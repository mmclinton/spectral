use std::env;
use std::path::Path;

// A build script has no error channel but a panic: either the UI compiles or
// there is nothing to link against.
#[expect(clippy::expect_used, reason = "build failure must abort the build")]
fn main() {
    let manifest_dir =
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo");

    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent-dark".into())
        .with_include_paths(vec![Path::new(&manifest_dir).join("ui")]);

    println!("cargo:rerun-if-changed=ui/");

    slint_build::compile_with_config("ui/app-window.slint", config).expect("Slint build failed");
}
