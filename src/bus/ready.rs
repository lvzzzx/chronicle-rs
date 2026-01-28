use std::path::Path;

/// Mark an endpoint as ready by creating a READY marker file.
///
/// This is an atomic operation using write-then-rename to ensure the marker
/// appears atomically to polling processes.
pub fn mark_ready(endpoint_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(endpoint_dir)?;
    let tmp_path = endpoint_dir.join("READY.tmp");
    let ready_path = endpoint_dir.join("READY");
    std::fs::write(&tmp_path, b"")?;
    std::fs::rename(tmp_path, ready_path)?;
    Ok(())
}

/// Check if an endpoint is ready by checking for the READY marker file.
pub fn is_ready(endpoint_dir: &Path) -> bool {
    let ready_path = endpoint_dir.join("READY");
    std::fs::metadata(ready_path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}
