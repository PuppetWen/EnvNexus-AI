use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Windows `fs::canonicalize` returns verbatim paths such as `\\?\C:\...`
/// and `\\?\UNC\server\share`. They are useful internally but should never be
/// persisted, displayed, or written to user environment variables.
pub fn simplify(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{stripped}"))
    } else if let Some(stripped) = text.strip_prefix(r"\\?\") {
        PathBuf::from(stripped.to_string())
    } else {
        path
    }
}

pub fn canonicalize_simplified(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map(simplify)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_verbatim_disk_prefix() {
        assert_eq!(
            simplify(PathBuf::from(r"\\?\C:\tools\python")),
            PathBuf::from(r"C:\tools\python")
        );
    }

    #[test]
    fn strips_verbatim_unc_prefix() {
        assert_eq!(
            simplify(PathBuf::from(r"\\?\UNC\server\share\dir")),
            PathBuf::from(r"\\server\share\dir")
        );
    }

    #[test]
    fn leaves_plain_paths_untouched() {
        assert_eq!(
            simplify(PathBuf::from(r"D:\plain\path")),
            PathBuf::from(r"D:\plain\path")
        );
    }

    #[test]
    fn canonicalize_simplified_returns_plain_form() {
        let canonical = canonicalize_simplified(&std::env::temp_dir()).unwrap();
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
    }
}
