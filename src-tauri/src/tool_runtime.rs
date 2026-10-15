use std::path::Path;

use crate::error::{AppError, AppResult};

/// The Windows Platform-Tools package keeps these libraries beside adb.exe.
/// Check them without starting a process, including when a version was cached.
pub fn validate_runtime_dependencies(tool_id: &str, executable: &Path) -> AppResult<()> {
    if tool_id != "adb" {
        return Ok(());
    }
    let directory = executable
        .parent()
        .ok_or_else(|| AppError::UnsafePath(executable.to_path_buf()))?;
    let missing = ["AdbWinApi.dll", "AdbWinUsbApi.dll"]
        .into_iter()
        .filter(|name| !directory.join(name).is_file())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(AppError::Message(format!(
        "ADB 安装目录不完整（{}）：缺少 {}。请完整解压官方 Platform-Tools 包，将 adb.exe 和配套 DLL 保留在同一目录。",
        directory.display(),
        missing.join("、")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn adb_runtime_check_requires_both_companion_libraries() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("adb.exe");
        fs::write(&executable, b"test fixture").unwrap();
        let error = validate_runtime_dependencies("adb", &executable)
            .unwrap_err()
            .to_string();
        assert!(error.contains("AdbWinApi.dll"));
        assert!(error.contains("AdbWinUsbApi.dll"));
        assert!(error.contains(&temp.path().display().to_string()));

        fs::write(temp.path().join("AdbWinApi.dll"), b"api").unwrap();
        let error = validate_runtime_dependencies("adb", &executable)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("AdbWinApi.dll"));
        assert!(error.contains("AdbWinUsbApi.dll"));

        fs::write(temp.path().join("AdbWinUsbApi.dll"), b"usb").unwrap();
        assert!(validate_runtime_dependencies("adb", &executable).is_ok());
    }

    #[test]
    fn unrelated_tools_do_not_require_adb_libraries() {
        assert!(validate_runtime_dependencies("node", Path::new("node.exe")).is_ok());
    }
}
