use std::path::Path;

pub fn mark_ready(endpoint_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(endpoint_dir)?;
    let tmp_path = endpoint_dir.join("READY.tmp");
    let ready_path = endpoint_dir.join("READY");
    std::fs::write(&tmp_path, b"")?;
    std::fs::rename(tmp_path, ready_path)?;
    Ok(())
}
