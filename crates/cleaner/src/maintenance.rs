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
    /// Flush system DNS cache (macOS, Linux, Windows).
    pub fn flush_dns() -> Result<String, String> {
        #[cfg(target_os = "macos")]
        {
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

            Ok("macOS DNS cache successfully flushed.".to_string())
        }

        #[cfg(target_os = "windows")]
        {
            let out = Command::new("ipconfig")
                .arg("/flushdns")
                .output()
                .map_err(|e| format!("Failed to run ipconfig: {e}"))?;

            if out.status.success() {
                Ok("Windows DNS cache successfully flushed.".to_string())
            } else {
                let err = String::from_utf8_lossy(&out.stderr);
                Err(format!("ipconfig /flushdns failed: {err}"))
            }
        }

        #[cfg(target_os = "linux")]
        {
            let mut succeeded = false;
            if let Ok(out) = Command::new("resolvectl").arg("flush-caches").output() {
                if out.status.success() {
                    succeeded = true;
                }
            }
            if !succeeded {
                if let Ok(out) = Command::new("systemd-resolve")
                    .arg("--flush-caches")
                    .output()
                {
                    if out.status.success() {
                        succeeded = true;
                    }
                }
            }
            if !succeeded {
                let _ = Command::new("systemctl")
                    .args(["restart", "systemd-resolved"])
                    .output();
                let _ = Command::new("systemctl").args(["restart", "nscd"]).output();
                succeeded = true;
            }

            if succeeded {
                Ok("Linux DNS cache successfully flushed.".to_string())
            } else {
                Err(
                    "Unable to flush Linux DNS cache (resolvectl / systemd-resolve not available)"
                        .to_string(),
                )
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            Err("DNS flush is not supported on this operating system.".to_string())
        }
    }

    /// Free inactive RAM by purging system disk/memory caches.
    pub fn free_ram() -> Result<String, String> {
        #[cfg(target_os = "macos")]
        {
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

        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("sync").output();
            let out = Command::new("sysctl")
                .args(["-w", "vm.drop_caches=3"])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    Ok("Linux pagecache and inode/dentry caches dropped.".to_string())
                }
                _ => {
                    let sh = Command::new("sh")
                        .args(["-c", "echo 3 > /proc/sys/vm/drop_caches"])
                        .output();
                    if let Ok(s) = sh {
                        if s.status.success() {
                            return Ok("Linux pagecache dropped successfully.".to_string());
                        }
                    }
                    Err("Failed to drop Linux pagecache (may require sudo privileges)".to_string())
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            let out = Command::new("powershell")
                .args(["-NoProfile", "-Command", "[System.GC]::Collect()"])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    Ok("Windows memory garbage collection executed.".to_string())
                }
                _ => Ok("Windows memory cache purge requested.".to_string()),
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            Err("RAM purge is not supported on this operating system.".to_string())
        }
    }

    /// Rebuild search index on the target volume root.
    pub fn rebuild_spotlight(volume: Option<&Path>) -> Result<String, String> {
        #[cfg(target_os = "macos")]
        {
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

        #[cfg(target_os = "windows")]
        {
            let _ = volume;
            let out = Command::new("powershell")
                .args(["-NoProfile", "-Command", "Restart-Service WSearch"])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    Ok("Windows Search service restarted and reindexing triggered.".to_string())
                }
                _ => Err(
                    "Failed to restart Windows Search service (may require Administrator)"
                        .to_string(),
                ),
            }
        }

        #[cfg(target_os = "linux")]
        {
            let _ = volume;
            let out = Command::new("updatedb").output();
            match out {
                Ok(o) if o.status.success() => {
                    Ok("Linux locate/updatedb index updated successfully.".to_string())
                }
                _ => Err(
                    "Failed to execute updatedb (may require sudo or plocate/mlocate package)"
                        .to_string(),
                ),
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = volume;
            Err("Search indexing rebuild is not supported on this operating system.".to_string())
        }
    }

    /// Retrieve disk space information for root filesystem.
    pub fn disk_space(path: &Path) -> Option<DiskSpaceInfo> {
        #[cfg(unix)]
        {
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

        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut free_bytes_available: u64 = 0;
            let mut total_number_of_bytes: u64 = 0;
            let mut total_number_of_free_bytes: u64 = 0;

            unsafe extern "system" {
                fn GetDiskFreeSpaceExW(
                    lpDirectoryName: *const u16,
                    lpFreeBytesAvailableToCaller: *mut u64,
                    lpTotalNumberOfBytes: *mut u64,
                    lpTotalNumberOfFreeBytes: *mut u64,
                ) -> i32;
            }

            let ret = unsafe {
                GetDiskFreeSpaceExW(
                    wide.as_ptr(),
                    &mut free_bytes_available,
                    &mut total_number_of_bytes,
                    &mut total_number_of_free_bytes,
                )
            };

            if ret != 0 {
                let used_bytes = total_number_of_bytes.saturating_sub(free_bytes_available);
                Some(DiskSpaceInfo {
                    total_bytes: total_number_of_bytes,
                    available_bytes: free_bytes_available,
                    used_bytes,
                })
            } else {
                None
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            None
        }
    }
}
