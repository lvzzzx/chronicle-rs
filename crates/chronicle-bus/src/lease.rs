use std::path::Path;

pub fn write_lease(endpoint_dir: &Path, payload: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(endpoint_dir)?;
    let lease_path = endpoint_dir.join("LEASE");
    std::fs::write(lease_path, payload)
}
