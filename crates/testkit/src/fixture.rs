//! Builds real directory trees for integration tests.

use std::path::Path;

/// Create files and directories under `base` from relative specs. A trailing
/// `/` creates a directory; other entries are files whose content is their
/// own relative path, so sizes are deterministic and non-zero.
pub fn create_tree(base: &Path, specs: &[&str]) -> std::io::Result<()> {
    for spec in specs {
        if let Some(dir) = spec.strip_suffix('/') {
            std::fs::create_dir_all(base.join(dir))?;
        } else {
            let p = base.join(spec);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, spec.as_bytes())?;
        }
    }
    Ok(())
}
