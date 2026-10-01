use std::{error::Error, fs::OpenOptions, io::Write};
use mmdbridge_core::{AssetType, Library};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let source = arguments.next().ok_or("usage: render_thumbnail <source> <new-output.webp>")?;
    let output = arguments.next().ok_or("missing output path")?;
    let library = Library::in_memory()?;
    let thumbnail = if let Some(model) = arguments.next() {
        library.set_motion_preview_model(Some(model.to_str().ok_or("invalid model path")?))?;
        let source = std::fs::canonicalize(source)?;
        let root = library.add_root_with_recursive(AssetType::Motion,
            source.parent().ok_or("missing parent")?.to_str().ok_or("invalid source path")?,
            Some("isolated motion probe"), false)?;
        library.enqueue_scan(&root.id)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let state = library.list_scan_states()?.into_iter().find(|state| state.root_id == root.id);
            if let Some(state) = state {
                if state.status == "Completed" { break; }
                if matches!(state.status.as_str(), "Failed" | "Cancelled") { return Err("isolated scan failed".into()); }
            }
            if std::time::Instant::now() >= deadline { return Err("isolated scan timed out".into()); }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let asset = library.list_assets(Some(AssetType::Motion), None, 50000)?
            .into_iter().find(|asset| std::path::Path::new(&asset.primary_source) == source)
            .ok_or("motion source was not discovered")?;
        library.render_thumbnail(&asset.id)?
    } else {
        library.render_thumbnail_file(source)?
    };
    OpenOptions::new().write(true).create_new(true).open(output)?
        .write_all(&thumbnail.preview_webp)?;
    println!("{}", serde_json::to_string_pretty(&thumbnail.report)?);
    Ok(())
}
