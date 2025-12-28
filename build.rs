use std::env;
use std::fs;
use std::path::Path;

fn main() {
    unsafe { env::set_var("SLINT_STYLE", "fluent-dark") };

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = env::var("OUT_DIR").unwrap();
    let fonts_dir = Path::new(&manifest_dir).join("fonts");

    let find_font = |base_name: &str| -> Option<std::path::PathBuf> {
        [format!("{}.otf", base_name), format!("{}.ttf", base_name)]
            .iter()
            .map(|name| fonts_dir.join(name))
            .find(|p| p.exists())
    };

    let regular_font = find_font("BerkeleyMono-Regular");
    let bold_font = find_font("BerkeleyMono-Bold");

    let ui_source = fs::read_to_string(Path::new(&manifest_dir).join("ui/app-window.slint"))
        .expect("Failed to read app-window.slint");

    let mut font_imports = Vec::new();
    let mut found_fonts = Vec::new();

    if let Some(path) = &regular_font {
        font_imports.push(format!("import \"{}\";", path.display()));
        found_fonts.push("Regular");
    }
    if let Some(path) = &bold_font {
        font_imports.push(format!("import \"{}\";", path.display()));
        found_fonts.push("Bold");
    }

    let font_import = if !font_imports.is_empty() {
        println!("cargo:rustc-cfg=has_berkeley_mono");
        println!(
            "cargo:warning=Building with embedded Berkeley Mono fonts: {}",
            found_fonts.join(", ")
        );
        font_imports.join("\n")
    } else {
        println!(
            "cargo:warning=Berkeley Mono fonts not found in {}. \
             The application will fall back to system fonts. \
             See fonts/README.md for setup instructions.",
            fonts_dir.display()
        );
        "// Fonts not available - using system fallback".to_string()
    };

    let generated_ui = ui_source.replace("// @FONT_IMPORT_PLACEHOLDER@", &font_import);

    let generated_ui_path = Path::new(&out_dir).join("app-window-generated.slint");
    fs::write(&generated_ui_path, generated_ui).expect("Failed to write generated UI file");

    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent-dark".into())
        .with_include_paths(vec![Path::new(&manifest_dir).join("ui")])
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);

    println!("cargo:rerun-if-changed=fonts/");
    println!("cargo:rerun-if-changed=ui/");

    slint_build::compile_with_config(&generated_ui_path, config).expect("Slint build failed");
}
