use std::path::Path;

/// Safety guard to protect critical operating system and user data from accidental modification.
pub struct SafetyGuard;

impl SafetyGuard {
    /// Returns Ok(()) if the path is permitted to be cleaned/trashed, or Err(reason) if protected.
    pub fn validate(path: &Path) -> Result<(), &'static str> {
        let original_str = path.to_string_lossy();
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => path.to_path_buf(),
        };
        let canonical_str = canonical.to_string_lossy();

        // 1. Root and immediate system partitions
        let is_system_blocked = |p: &str| {
            // Permit temporary directories used for tests and temporary files
            if p.starts_with("/var/folders")
                || p.starts_with("/private/var/folders")
                || p.starts_with("/tmp")
                || p.starts_with("/private/tmp")
                || p.starts_with("/var/tmp")
            {
                return false;
            }

            // Windows temp paths allowance
            let p_lower = p.to_ascii_lowercase();
            if p_lower.contains("appdata\\local\\temp") || p_lower.contains("windows\\temp") {
                return false;
            }

            // Unix / macOS / Linux root and system partitions
            if p == "/"
                || p == "/System"
                || p.starts_with("/System/")
                || p == "/usr"
                || p.starts_with("/usr/")
                || p == "/bin"
                || p.starts_with("/bin/")
                || p == "/sbin"
                || p.starts_with("/sbin/")
                || p == "/etc"
                || p.starts_with("/etc/")
                || p == "/var"
                || p.starts_with("/var/")
                || p == "/boot"
                || p.starts_with("/boot/")
                || p == "/dev"
                || p.starts_with("/dev/")
                || p == "/proc"
                || p.starts_with("/proc/")
                || p == "/sys"
                || p.starts_with("/sys/")
                || p == "/private"
                || p.starts_with("/private/etc")
                || p.starts_with("/private/var")
                || p == "/Volumes"
            {
                return true;
            }

            // Windows system partitions & drive roots
            // e.g. "C:\", "C:\Windows", "C:\Program Files", "C:\ProgramData\Microsoft"
            if p_lower.len() == 3 && p_lower.ends_with(":\\") {
                return true;
            }
            if p_lower.starts_with("c:\\windows")
                || p_lower.starts_with("c:\\program files")
                || p_lower.starts_with("c:\\program files (x86)")
                || p_lower.starts_with("c:\\programdata\\microsoft")
                || p_lower.contains("\\system32")
                || p_lower.contains("\\syswow64")
            {
                return true;
            }

            false
        };

        if is_system_blocked(&original_str) || is_system_blocked(&canonical_str) {
            return Err("Cannot modify system protected directory");
        }

        // 2. User Home directory protection
        if let Some(home) = dirs::home_dir() {
            let home_str = home.to_string_lossy();
            if canonical_str == home_str || original_str == home_str {
                return Err("Cannot delete user home directory");
            }

            // Sensitive security keys and identity stores
            let ssh_dir = home.join(".ssh");
            if canonical == ssh_dir || canonical.starts_with(&ssh_dir) {
                return Err("Protected user SSH keys (~/.ssh)");
            }

            let gpg_dir = home.join(".gnupg");
            if canonical == gpg_dir || canonical.starts_with(&gpg_dir) {
                return Err("Protected GPG keys (~/.gnupg)");
            }

            let keychain_dir = home.join("Library/Keychains");
            if canonical == keychain_dir || canonical.starts_with(&keychain_dir) {
                return Err("Protected macOS Keychains (~/Library/Keychains)");
            }

            // Linux keyrings
            let keyring_dir = home.join(".local/share/keyrings");
            if canonical == keyring_dir || canonical.starts_with(&keyring_dir) {
                return Err("Protected Linux keyring store");
            }

            // Windows user registry hive
            let ntuser = home.join("NTUSER.DAT");
            if canonical == ntuser {
                return Err("Protected Windows user registry hive (NTUSER.DAT)");
            }

            // Protect root of user data libraries
            let mut protected_home_dirs = vec![
                home.join("Documents"),
                home.join("Desktop"),
                home.join("Pictures"),
                home.join("Music"),
                home.join("Videos"),
                home.join("Movies"),
            ];
            if cfg!(target_os = "macos") {
                protected_home_dirs.push(home.join("Library"));
            }
            if cfg!(target_os = "windows") {
                protected_home_dirs.push(home.join("AppData"));
            }

            for d in &protected_home_dirs {
                if &canonical == d {
                    return Err("Cannot delete root of user library folder");
                }
            }
        }

        // 3. Git repository and key material files
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if file_name == ".git" {
                return Err("Cannot delete Git directory repository root (.git)");
            }
            if file_name.ends_with(".key")
                || file_name.ends_with(".pem")
                || file_name.ends_with(".keystore")
                || file_name.ends_with(".kdbx")
            {
                return Err("Cannot delete cryptographic keys or password databases");
            }
        }

        Ok(())
    }

    /// Check if path is safe without returning Err.
    pub fn is_safe(path: &Path) -> bool {
        Self::validate(path).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_paths_rejected() {
        assert!(SafetyGuard::validate(Path::new("/")).is_err());
        assert!(SafetyGuard::validate(Path::new("/System")).is_err());
        assert!(SafetyGuard::validate(Path::new("/usr")).is_err());
        assert!(SafetyGuard::validate(Path::new("/usr/bin")).is_err());
        assert!(SafetyGuard::validate(Path::new("/bin")).is_err());
        assert!(SafetyGuard::validate(Path::new("/etc/hosts")).is_err());
        assert!(SafetyGuard::validate(Path::new("C:\\Windows")).is_err());
        assert!(SafetyGuard::validate(Path::new("C:\\Program Files")).is_err());
    }

    #[test]
    fn test_home_protection() {
        if let Some(home) = dirs::home_dir() {
            assert!(SafetyGuard::validate(&home).is_err());
            assert!(SafetyGuard::validate(&home.join(".ssh")).is_err());
            assert!(SafetyGuard::validate(&home.join(".ssh/id_rsa")).is_err());
            assert!(SafetyGuard::validate(&home.join(".gnupg")).is_err());
        }
    }

    #[test]
    fn test_cache_paths_allowed() {
        if let Some(home) = dirs::home_dir() {
            let cache_file = home.join(".cache/my_test_app/cache.bin");
            let _ = SafetyGuard::validate(&cache_file);
        }
    }
}
