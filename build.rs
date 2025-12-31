use std::env;
use std::path::Path;

fn main() {
    unsafe { env::set_var("SLINT_STYLE", "fluent-dark") };

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();

    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent-dark".into())
        .with_include_paths(vec![Path::new(&manifest_dir).join("ui")]);

    println!("cargo:rerun-if-changed=ui/");

    slint_build::compile_with_config("ui/app-window.slint", config).expect("Slint build failed");
}
