use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// `fs::canonicalize` 在 Windows 上返回 `\\?\C:\...` 或 `\\?\UNC\server\share` 形式的
/// verbatim 路径；写入 PATH、JAVA_HOME 或配置文件后会破坏 cmd、Gradle 等消费方。
/// 该函数在保持规范化结果的同时去掉 verbatim 前缀。
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

/// `fs::canonicalize` 后立即去掉 verbatim 前缀；所有会被持久化或写入
/// 用户环境的路径都应经过这里，而不是直接使用 `fs::canonicalize`。
pub fn canonicalize_simplified(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).map(simplify)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_verbatim_disk_prefix() {
        assert_eq!(
            simplify(PathBuf::from(r"\\?\C:\tools\java")),
            PathBuf::from(r"C:\tools\java")
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
        let temp = std::env::temp_dir();
        let canonical = canonicalize_simplified(&temp).unwrap();
        assert!(!canonical.to_string_lossy().starts_with(r"\\?\"));
    }
}
