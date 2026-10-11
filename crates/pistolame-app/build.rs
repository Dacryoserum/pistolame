//! Compiles the Slint UI files at build time.

fn main() {
    slint_build::compile("ui/main_window.slint").expect("Slint UI compilation must succeed");
}
