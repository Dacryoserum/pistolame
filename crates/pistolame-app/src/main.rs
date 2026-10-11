//! Pistolame desktop application: the Slint user interface.

use slint::ComponentHandle;

/// Holds the code generated from the `.slint` files, which we cannot annotate: lints that guard
/// hand-written code are relaxed here only.
#[allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::todo,
    clippy::dbg_macro
)]
mod generated_ui {
    slint::include_modules!();
}

use generated_ui::MainWindow;

fn main() -> anyhow::Result<()> {
    let main_window = MainWindow::new()?;
    main_window.run()?;
    Ok(())
}
