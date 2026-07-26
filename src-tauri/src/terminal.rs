use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    environment::{get_case_insensitive, read_environment, split_path},
    error::{AppError, AppResult},
    model::{EnvironmentScope, TerminalCommandStatus},
    plugins::PluginRegistry,
};

const OPERATIONS: [(&str, &str); 7] = [
    ("list", "list"),
    ("versions", "versions"),
    ("install", "install"),
    ("use", "use"),
    ("repair", "repair"),
    ("uninstall", "uninstall"),
    ("root", "root"),
];

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCommandPreferences {
    schema_version: u32,
    directory: PathBuf,
}

pub fn command_prefix(tool_id: &str) -> &str {
    match tool_id {
        "java" => "jdk",
        "node" => "node",
        "dotnet" => "dotnet",
        value => value,
    }
}

pub fn command_directory(data_root: &Path) -> AppResult<PathBuf> {
    let preferences_path = preferences_path(data_root);
    if !preferences_path.is_file() {
        return Ok(data_root.join("commands"));
    }
    let preferences =
        serde_json::from_slice::<TerminalCommandPreferences>(&fs::read(preferences_path)?)?;
    if preferences.schema_version != 1 || !preferences.directory.is_absolute() {
        return Err(AppError::Message(
            "命令脚本目录配置无效，请重新选择保存目录".to_string(),
        ));
    }
    Ok(preferences.directory)
}

pub fn status(registry: &PluginRegistry, data_root: &Path) -> AppResult<TerminalCommandStatus> {
    let directory = command_directory(data_root)?;
    let user = read_environment(EnvironmentScope::User)?;
    let enabled_in_user_path = split_path(get_case_insensitive(&user, "PATH"))
        .iter()
        .any(|entry| same_path(entry, &directory));
    let script_count = fs::read_dir(&directory)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case("cmd"))
                })
                .count()
        })
        .unwrap_or(0);
    Ok(TerminalCommandStatus {
        directory,
        enabled_in_user_path,
        script_count,
        expected_script_count: registry.all().len() * OPERATIONS.len() + 4,
    })
}

pub fn prepare(registry: &PluginRegistry, data_root: &Path) -> AppResult<TerminalCommandStatus> {
    let directory = command_directory(data_root)?;
    fs::create_dir_all(&directory)?;
    let executable = std::env::current_exe()
        .map_err(|error| AppError::Message(format!("无法定位 EnvNexus AI 主程序：{error}")))?;
    let executable = escape_batch_path(&executable)?;

    for plugin in registry.all() {
        let tool_id = plugin.descriptor().id;
        let prefix = command_prefix(tool_id);
        for (command_name, operation) in OPERATIONS {
            let filename = format!("{prefix}-{command_name}.cmd");
            let body = if operation == "root" {
                // cmd 中 shift 不影响 %*，不能用 %* 传剩余参数；
                // 逐个展开 %2..%9，缺省参数会展开为空。
                format!(
                    "@echo off\r\n@\"{executable}\" root \"%~1\" \"{tool_id}\" %2 %3 %4 %5 %6 %7 %8 %9\r\n@exit /b %errorlevel%\r\n"
                )
            } else {
                format!(
                    "@echo off\r\n@\"{executable}\" {operation} \"{tool_id}\" %*\r\n@exit /b %errorlevel%\r\n"
                )
            };
            write_atomic(&directory.join(filename), body.as_bytes())?;
        }
    }

    for (filename, arguments) in [
        ("env-tools.cmd", "tools %*"),
        ("env-scan.cmd", "scan %*"),
        ("env-diagnose.cmd", "diagnose %*"),
        ("env-repair.cmd", "diagnostic-repair %*"),
    ] {
        let body =
            format!("@echo off\r\n@\"{executable}\" {arguments}\r\n@exit /b %errorlevel%\r\n");
        write_atomic(&directory.join(filename), body.as_bytes())?;
    }
    status(registry, data_root)
}

pub fn save_directory(
    registry: &PluginRegistry,
    data_root: &Path,
    directory: PathBuf,
) -> AppResult<TerminalCommandStatus> {
    let directory = normalize_command_directory(directory)?;
    let current = command_directory(data_root)?;
    let user = read_environment(EnvironmentScope::User)?;
    let current_is_enabled = split_path(get_case_insensitive(&user, "PATH"))
        .iter()
        .any(|entry| same_path(entry, &current));
    if current_is_enabled && !same_path(&directory.to_string_lossy(), &current) {
        return Err(AppError::Message(
            "当前命令脚本目录仍在用户 PATH 中；请先预览并确认停用，再更换目录".to_string(),
        ));
    }

    let preferences = TerminalCommandPreferences {
        schema_version: 1,
        directory,
    };
    let path = preferences_path(data_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&preferences)?;
    write_atomic(&path, &bytes)?;
    status(registry, data_root)
}

fn preferences_path(data_root: &Path) -> PathBuf {
    data_root.join("config").join("terminal-commands.json")
}

fn normalize_command_directory(path: PathBuf) -> AppResult<PathBuf> {
    let text = path.to_string_lossy();
    if !path.is_absolute() || path.parent().is_none() || text.contains([';', '%', '\r', '\n', '"'])
    {
        return Err(AppError::Message(
            "命令脚本目录必须是非磁盘根目录的绝对路径，且不能包含 ; % 或引号".to_string(),
        ));
    }
    fs::create_dir_all(&path)?;
    let canonical = crate::paths::canonicalize_simplified(&path)?;
    if canonical.parent().is_none() {
        return Err(AppError::Message(
            "不能把磁盘根目录作为命令脚本目录".to_string(),
        ));
    }
    Ok(canonical)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| AppError::UnsafePath(path.to_path_buf()))?
        .to_string_lossy();
    let temporary = path.with_file_name(format!("{file_name}.tmp"));
    fs::write(&temporary, bytes)?;
    if !path.exists() {
        fs::rename(temporary, path)?;
        return Ok(());
    }

    let previous = path.with_file_name(format!("{file_name}.previous"));
    if previous.exists() {
        fs::remove_file(&previous)?;
    }
    fs::rename(path, &previous)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::rename(&previous, path);
        return Err(error.into());
    }
    fs::remove_file(previous)?;
    Ok(())
}

fn escape_batch_path(path: &Path) -> AppResult<String> {
    let value = path.to_string_lossy();
    if value.contains(['\r', '\n', '"']) {
        return Err(AppError::UnsafePath(path.to_path_buf()));
    }
    Ok(value.replace('%', "%%"))
}

fn same_path(value: &str, path: &Path) -> bool {
    normalize(value) == normalize(&path.to_string_lossy())
}

fn normalize(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_start_matches(r"\\?\")
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_expected_public_command_prefixes() {
        assert_eq!(command_prefix("java"), "jdk");
        assert_eq!(command_prefix("dotnet"), "dotnet");
        assert_eq!(command_prefix("python"), "python");
    }

    #[test]
    fn prepares_every_tool_command_without_a_second_executable() {
        let data = tempfile::tempdir().unwrap();
        let registry = PluginRegistry::builtin();
        let status = prepare(&registry, data.path()).unwrap();
        assert_eq!(status.script_count, registry.all().len() * 7 + 4);
        assert_eq!(status.script_count, status.expected_script_count);

        let java_list = fs::read_to_string(status.directory.join("jdk-list.cmd")).unwrap();
        assert!(java_list.to_ascii_lowercase().contains("envnexus"));
        assert!(java_list.contains(" list \"java\" %*"));
        assert!(!status.directory.join("envpilot-cli.exe").exists());
    }

    #[test]
    fn saves_and_reuses_a_custom_command_directory() {
        let data = tempfile::tempdir().unwrap();
        let custom = data.path().join("custom-command-scripts");
        let registry = PluginRegistry::builtin();

        let saved = save_directory(&registry, data.path(), custom.clone()).unwrap();
        assert_eq!(
            saved.directory,
            crate::paths::canonicalize_simplified(&custom).unwrap()
        );
        assert_eq!(saved.script_count, 0);

        let prepared = prepare(&registry, data.path()).unwrap();
        assert_eq!(
            prepared.directory,
            crate::paths::canonicalize_simplified(&custom).unwrap()
        );
        assert_eq!(prepared.script_count, prepared.expected_script_count);
        assert!(custom.join("jdk-root.cmd").is_file());
    }
}
