//! Compiles the desktop interface and embeds its portable assets; on Windows, also embeds the
//! app icon into the executable (Explorer, taskbar, Alt+Tab).
fn main() {
    let configuration = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/main_window.slint", configuration)
        .expect("Slint UI compilation must succeed");
    #[cfg(windows)]
    embed_windows_icon();
}

#[cfg(windows)]
fn embed_windows_icon() {
    println!("cargo:rerun-if-changed=assets/app-icon/pistolame.ico");
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/app-icon/pistolame.ico");
    resource
        .compile()
        .expect("the Windows resource compiler must embed the app icon");
}
