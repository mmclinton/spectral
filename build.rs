use std::env;

fn main() {
    unsafe { env::set_var("SLINT_STYLE", "fluent-dark") };
    slint_build::compile("ui/app-window.slint").expect("Slint build failed");
}
