use std::path::Path;

/// Safety guard to protect critical macOS and user data from accidental modification.
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
            {
                return false;
            }

            p == "/"
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
                || p == "/private"
                || p.starts_with("/private/etc")
                || p.starts_with("/private/var")
                || p == "/dev"
                || p.starts_with("/dev/")
                || p == "/Volumes"
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

            // Protect root of user data libraries
            let protected_home_dirs = [
                home.join("Documents"),
                home.join("Desktop"),
                home.join("Pictures"),
                home.join("Music"),
                home.join("Movies"),
                home.join("Library"),
            ];

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
                return Err("Protected cryptographic key or database file");
            }
        }

        Ok(())
    }

    /// Check if path is safe, returning a boolean.
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
        assert!(SafetyGuard::validate(Path::new("/System/Library")).is_err());
        assert!(SafetyGuard::validate(Path::new("/usr/bin")).is_err());
    }

    #[test]
    fn test_home_protection() {
        if let Some(home) = dirs::home_dir() {
            assert!(SafetyGuard::validate(&home).is_err());
            assert!(SafetyGuard::validate(&home.join(".ssh")).is_err());
            assert!(SafetyGuard::validate(&home.join(".ssh/id_rsa")).is_err());
            assert!(SafetyGuard::validate(&home.join("Library/Keychains")).is_err());
        }
    }

    #[test]
    fn test_cache_paths_allowed() {
        if let Some(home) = dirs::home_dir() {
            assert!(SafetyGuard::validate(&home.join("Library/Caches/Google/Chrome")).is_ok());
            assert!(SafetyGuard::validate(&home.join(".cargo/registry/cache")).is_ok());
            assert!(SafetyGuard::validate(&home.join(".npm/_cacache")).is_ok());
        }
    }
}
