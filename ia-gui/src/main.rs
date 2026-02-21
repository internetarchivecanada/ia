slint::include_modules!();

fn main() -> anyhow::Result<()> {
    let app = AppWindow::new()?;
    app.run()?;
    Ok(())
}
