//! Compiles the desktop interface and embeds its portable assets.
fn main() {
    let configuration = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/main_window.slint", configuration)
        .expect("Slint UI compilation must succeed");
}
