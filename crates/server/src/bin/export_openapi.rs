//! Offline contract generation needs neither a database nor model credentials.
fn main() -> anyhow::Result<()> {
    let document = server::openapi().to_pretty_json()?;
    if let Some(path) = std::env::args_os().nth(1) {
        let path = std::path::Path::new(&path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, format!("{document}\n"))?;
    } else {
        println!("{document}");
    }
    Ok(())
}
