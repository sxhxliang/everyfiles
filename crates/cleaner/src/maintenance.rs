use std::path::Path;
use std::process::Command;

/// System performance maintenance tasks.
pub struct SystemMaintenance;

#[derive(Clone, Debug)]
pub struct DiskSpaceInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
}

impl SystemMaintenance {
    /// Flush macOS DNS cache.
    pub fn flush_dns() -> Result<String, String> {
        let out1 = Command::new("dscacheutil")
            .arg("-flushcache")
            .output()
            .map_err(|e| format!("Failed to run dscacheutil: {e}"))?;

        if !out1.status.success() {
            return Err("dscacheutil returned non-zero exit code".to_string());
        }

        let _ = Command::new("killall")
            .args(["-HUP", "mDNSResponder"])
            .output();

        Ok("DNS cache successfully flushed.".to_string())
    }

    /// Free inactive RAM by purging system disk/memory caches.
    pub fn free_ram() -> Result<String, String> {
        let out = Command::new("purge")
            .output()
            .map_err(|e| format!("Failed to execute purge: {e}. (May require sudo)"))?;

        if out.status.success() {
            Ok("RAM disk caches successfully purged.".to_string())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(format!("purge failed: {stderr}"))
        }
    }

    /// Rebuild Spotlight index on the target volume root.
    pub fn rebuild_spotlight(volume: Option<&Path>) -> Result<String, String> {
        let target = volume.unwrap_or_else(|| Path::new("/"));
        let out = Command::new("mdutil")
            .args(["-E", &target.to_string_lossy()])
            .output()
            .map_err(|e| format!("Failed to execute mdutil: {e}. (May require sudo)"))?;

        if out.status.success() {
            Ok(format!(
                "Spotlight index reindexing initiated for {target:?}"
            ))
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(format!("mdutil failed: {stderr}"))
        }
    }

    /// Retrieve disk space information for root filesystem.
    #[cfg(unix)]
    pub fn disk_space(path: &Path) -> Option<DiskSpaceInfo> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };

        if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } == 0 {
            let bsize = stat.f_frsize as u64;
            let total_bytes = stat.f_blocks as u64 * bsize;
            let available_bytes = stat.f_bavail as u64 * bsize;
            let used_bytes = total_bytes.saturating_sub(available_bytes);
            Some(DiskSpaceInfo {
                total_bytes,
                available_bytes,
                used_bytes,
            })
        } else {
            None
        }
    }
}
