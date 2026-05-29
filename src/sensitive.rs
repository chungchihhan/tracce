use std::path::Path;

const SUFFIXES: &[&str] = &[
    ".pem", ".key", "id_rsa", "id_ed25519",
];

const EXACT: &[&str] = &[
    "/etc/passwd", "/etc/shadow", "/etc/sudoers",
];

const HOME_PREFIXES: &[&str] = &[
    ".aws/", ".ssh/", ".gnupg/", ".config/gh/", ".docker/config.json",
];

const BASENAMES: &[&str] = &[
    ".env",
];

const BASENAME_PREFIXES: &[&str] = &[
    ".env.", // .env.local, .env.production, etc.
];

pub fn is_sensitive(path: &Path) -> bool {
    let p = path.to_string_lossy();

    if EXACT.iter().any(|e| p == *e) {
        return true;
    }
    if SUFFIXES.iter().any(|s| p.ends_with(s)) {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().to_string();
        for prefix in HOME_PREFIXES {
            let full = format!("{home}/{prefix}");
            if p.starts_with(&full) {
                return true;
            }
        }
    }
    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
        if BASENAMES.iter().any(|b| name == *b) {
            return true;
        }
        if BASENAME_PREFIXES.iter().any(|b| name.starts_with(b)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn flags_etc_passwd() {
        assert!(is_sensitive(Path::new("/etc/passwd")));
    }

    #[test]
    fn flags_dotenv() {
        assert!(is_sensitive(Path::new("/x/y/.env")));
        assert!(is_sensitive(Path::new("/x/y/.env.production")));
    }

    #[test]
    fn flags_pem_anywhere() {
        assert!(is_sensitive(Path::new("/tmp/whatever.pem")));
    }

    #[test]
    fn ignores_innocuous() {
        assert!(!is_sensitive(Path::new("/tmp/foo.txt")));
        assert!(!is_sensitive(Path::new("/Users/x/Developer/peekaboo/README.md")));
    }

    #[test]
    fn flags_home_ssh() {
        std::env::set_var("HOME", "/Users/x");
        assert!(is_sensitive(Path::new("/Users/x/.ssh/id_rsa")));
    }
}
