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
    Ok(crate::paths::simplify(preferences.directory))
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
        expected_script_count: registry.all().len() * OPERATIONS.len() + 5,
    })
}

pub fn prepare(registry: &PluginRegistry, data_root: &Path) -> AppResult<TerminalCommandStatus> {
    let directory = command_directory(data_root)?;
    validate_command_directory(&directory)?;
    fs::create_dir_all(&directory)?;
    let executable = std::env::current_exe()
        .map_err(|error| AppError::Message(format!("无法定位 EnvNexus AI 主程序：{error}")))?;
    let executable = escape_batch_path(&executable)?;

    for plugin in registry.all() {
        let tool_id = plugin.descriptor().id;
        let prefix = command_prefix(tool_id);
        for (command_name, operation) in OPERATIONS {
            let filename = format!("{prefix}-{command_name}.cmd");
            let invocation = if operation == "root" {
                root_command_invocation(&executable, tool_id)
            } else {
                format!("@\"{executable}\" {operation} \"{tool_id}\" %*\r\n")
            };
            let body = utf8_command_script(&invocation);
            write_atomic(&directory.join(filename), body.as_bytes())?;
        }
    }

    for (filename, arguments) in [
        ("env-tools.cmd", "tools %*"),
        ("env-scan.cmd", "scan %*"),
        ("env-refresh.cmd", "refresh %*"),
        ("env-diagnose.cmd", "diagnose %*"),
        ("env-repair.cmd", "diagnostic-repair %*"),
    ] {
        let body = utf8_command_script(&format!("@\"{executable}\" {arguments}\r\n"));
        write_atomic(&directory.join(filename), body.as_bytes())?;
    }
    status(registry, data_root)
}

fn root_command_invocation(executable: &str, tool_id: &str) -> String {
    // SHIFT does not change %*. Parse the two root operands explicitly so the
    // action is not forwarded twice and global flags can appear on either side.
    format!(
        concat!(
            "@set \"_envnexus_root_action=\"\r\n",
            "@set \"_envnexus_root_path=\"\r\n",
            "@set \"_envnexus_root_json=\"\r\n",
            ":_envnexus_root_parse\r\n",
            "@if \"%~1\"==\"\" goto _envnexus_root_invoke\r\n",
            "@if \"%~1\"==\"--json\" goto _envnexus_root_json\r\n",
            "@if \"%~1\"==\"--yes\" goto _envnexus_root_next\r\n",
            "@if \"%~1\"==\"--refresh\" goto _envnexus_root_next\r\n",
            "@if not defined _envnexus_root_action goto _envnexus_root_action\r\n",
            "@if not defined _envnexus_root_path goto _envnexus_root_path\r\n",
            ":_envnexus_root_next\r\n",
            "@shift\r\n@goto _envnexus_root_parse\r\n",
            ":_envnexus_root_json\r\n",
            "@set \"_envnexus_root_json=--json\"\r\n@goto _envnexus_root_next\r\n",
            ":_envnexus_root_action\r\n",
            "@set \"_envnexus_root_action=%~1\"\r\n@goto _envnexus_root_next\r\n",
            ":_envnexus_root_path\r\n",
            "@set \"_envnexus_root_path=%~1\"\r\n@goto _envnexus_root_next\r\n",
            ":_envnexus_root_invoke\r\n",
            "@if defined _envnexus_root_path goto _envnexus_root_with_path\r\n",
            "@\"{executable}\" root \"%_envnexus_root_action%\" \"{tool_id}\" %_envnexus_root_json%\r\n",
            "@goto _envnexus_root_done\r\n",
            ":_envnexus_root_with_path\r\n",
            "@\"{executable}\" root \"%_envnexus_root_action%\" \"{tool_id}\" \"%_envnexus_root_path%\" %_envnexus_root_json%\r\n",
            ":_envnexus_root_done\r\n"
        ),
        executable = executable,
        tool_id = tool_id,
    )
}

/// The ASCII prelude is readable under the caller's original code page. CMD
/// reads the Unicode invocation only after switching to UTF-8, then restores
/// the caller's page and exit code on ordinary success or failure. A Ctrl+C
/// termination of the batch interpreter cannot provide a finally guarantee.
fn utf8_command_script(invocation: &str) -> String {
    format!(
        concat!(
            "@echo off\r\n",
            "@setlocal DisableDelayedExpansion\r\n",
            "@set \"_envnexus_codepage=\"\r\n",
            "@for /f \"tokens=2 delims=:\" %%C in ('chcp 2^>nul') do @for /f \"tokens=1\" %%P in (\"%%C\") do @set \"_envnexus_codepage=%%P\"\r\n",
            "@for /f \"delims=0123456789\" %%C in (\"%_envnexus_codepage%\") do @set \"_envnexus_codepage=\"\r\n",
            "@if not defined _envnexus_codepage goto _envnexus_capture_failed\r\n",
            "@if \"%_envnexus_codepage%\"==\"65001\" goto _envnexus_run\r\n",
            "@chcp 65001 >nul\r\n",
            "@if errorlevel 1 goto _envnexus_switch_failed\r\n",
            ":_envnexus_run\r\n",
            "{invocation}",
            "@set \"_envnexus_exit=%errorlevel%\"\r\n",
            "@if \"%_envnexus_codepage%\"==\"65001\" goto _envnexus_return\r\n",
            "@chcp %_envnexus_codepage% >nul\r\n",
            "@if errorlevel 1 goto _envnexus_restore_failed\r\n",
            ":_envnexus_return\r\n",
            "@endlocal & exit /b %_envnexus_exit%\r\n",
            ":_envnexus_capture_failed\r\n",
            "@echo EnvNexus AI: cannot read the console code page; command was not run. 1>&2\r\n",
            "@endlocal & exit /b 1\r\n",
            ":_envnexus_switch_failed\r\n",
            "@chcp %_envnexus_codepage% >nul 2>&1\r\n",
            "@echo EnvNexus AI: cannot select UTF-8; command was not run. 1>&2\r\n",
            "@endlocal & exit /b 1\r\n",
            ":_envnexus_restore_failed\r\n",
            "@echo EnvNexus AI: could not restore console code page %_envnexus_codepage%. 1>&2\r\n",
            "@endlocal & exit /b 1\r\n"
        ),
        invocation = invocation,
    )
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
    validate_command_directory(&path)?;
    fs::create_dir_all(&path)?;
    let canonical = crate::paths::canonicalize_simplified(&path)?;
    if canonical.parent().is_none() {
        return Err(AppError::Message(
            "不能把磁盘根目录作为命令脚本目录".to_string(),
        ));
    }
    Ok(canonical)
}

fn validate_command_directory(path: &Path) -> AppResult<()> {
    let text = path.to_string_lossy();
    if !path.is_absolute() || path.parent().is_none() || text.contains([';', '%', '\r', '\n', '"'])
    {
        return Err(AppError::Message(
            "命令脚本目录必须是非磁盘根目录的绝对路径，且不能包含 ; % 或引号；请在终端命令设置中选择兼容的保存目录".to_string(),
        ));
    }
    Ok(())
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
        assert_eq!(status.script_count, registry.all().len() * 7 + 5);
        assert_eq!(status.script_count, status.expected_script_count);

        let java_list = fs::read_to_string(status.directory.join("jdk-list.cmd")).unwrap();
        assert!(java_list.to_ascii_lowercase().contains("envnexus"));
        assert!(java_list.contains(" list \"java\" %*"));
        assert!(java_list.starts_with("@echo off\r\n@setlocal DisableDelayedExpansion\r\n"));
        assert!(java_list.contains("@chcp %_envnexus_codepage% >nul"));
        assert!(!java_list.starts_with('\u{feff}'));
        assert!(status.directory.join("env-refresh.cmd").is_file());
        assert!(!status.directory.join("envpilot-cli.exe").exists());
        let python_root = fs::read_to_string(status.directory.join("python-root.cmd")).unwrap();
        assert!(!python_root.contains("%*"));
        assert!(
            python_root
                .contains("root \"%_envnexus_root_action%\" \"python\" \"%_envnexus_root_path%\"")
        );
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

    #[test]
    fn default_command_directory_validation_does_not_break_read_only_status() {
        let data = tempfile::tempdir().unwrap();
        let unsupported = data.path().join("data%literal%");
        let registry = PluginRegistry::builtin();
        let current = status(&registry, &unsupported).unwrap();
        assert_eq!(current.directory, unsupported.join("commands"));
        assert_eq!(current.script_count, 0);
        assert!(
            !unsupported.exists(),
            "status must not create the directory"
        );
        let error = prepare(&registry, &unsupported).unwrap_err().to_string();
        assert!(error.contains("不能包含 ; % 或引号"));
        assert!(
            !unsupported.exists(),
            "prepare must validate before writing"
        );
    }

    #[test]
    fn utf8_script_has_ascii_guards_before_the_unicode_invocation() {
        let invocation =
            "@\"E:\\中文-日本語-é 空格 & 100%%!\\EnvNexus-AI.exe\" list \"java\" %*\r\n";
        let body = utf8_command_script(invocation);
        let (prelude, _) = body.split_once(invocation).unwrap();
        assert!(prelude.is_ascii());
        assert!(
            prelude.contains("if not defined _envnexus_codepage goto _envnexus_capture_failed")
        );
        assert!(prelude.contains("if errorlevel 1 goto _envnexus_switch_failed"));
        assert!(body.contains("@set \"_envnexus_exit=%errorlevel%\""));
        assert!(body.contains("@endlocal & exit /b %_envnexus_exit%"));
        assert!(body.contains("cannot read the console code page; command was not run"));
        assert!(body.contains("cannot select UTF-8; command was not run"));
        assert!(body.contains("could not restore console code page"));
    }

    #[cfg(windows)]
    #[test]
    fn unicode_terminal_script_fixture() {
        let Some(expected) = std::env::var_os("ENVNEXUS_TERMINAL_FIXTURE_EXE") else {
            return;
        };
        assert_eq!(
            crate::paths::canonicalize_simplified(&std::env::current_exe().unwrap()).unwrap(),
            crate::paths::canonicalize_simplified(Path::new(&expected)).unwrap()
        );
        assert_eq!(
            unsafe { windows_sys::Win32::System::Console::GetConsoleOutputCP() },
            65001
        );
        let args = std::env::args().collect::<Vec<_>>();
        // --skip accepts an arbitrary stable libtest string. This impossible
        // test-name filter lets the fixture inspect a real Unicode CLI operand.
        let logfile = args.windows(2).find(|pair| pair[0] == "--skip").unwrap();
        assert_eq!(
            logfile[1],
            std::env::var("ENVNEXUS_TERMINAL_FIXTURE_LOG").unwrap()
        );
        fs::write(&logfile[1], b"UNICODE_TERMINAL_ARGUMENT_OK").unwrap();
        println!("UNICODE_TERMINAL_FIXTURE_OK");
        if std::env::var("ENVNEXUS_TERMINAL_FIXTURE_FAIL").as_deref() == Ok("1") {
            std::process::exit(23);
        }
    }

    #[cfg(windows)]
    #[test]
    fn real_cmd_wrappers_preserve_unicode_paths_exit_codes_and_original_code_pages() {
        use std::{ffi::OsStr, time::Duration};

        let data = tempfile::tempdir().unwrap();
        let commands = data.path().join("中文-日本語-é commands & !bang!");
        fs::create_dir_all(&commands).unwrap();
        let binaries = data.path().join("中文-日本語-é space & %literal% !bang!");
        fs::create_dir_all(&binaries).unwrap();
        let executable = binaries.join("可信 EnvNexus & %literal% !bang!.exe");
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let escaped = escape_batch_path(&executable).unwrap();
        let script = commands.join("envnexus-unicode-fixture.cmd");
        fs::write(
            &script,
            utf8_command_script(&format!("@\"{escaped}\" %*\r\n")),
        )
        .unwrap();
        let cmd = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let path = format!(
            "{};{}",
            commands.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let original_fixture = std::env::var_os("ENVNEXUS_TERMINAL_FIXTURE_EXE");
        for code_page in [437, 65001] {
            for (fail, expected_exit) in [("0", 0), ("1", 23)] {
                let logfile = data
                    .path()
                    .join(format!("中文-日本語-é args & {code_page}-{fail}.log"));
                let harness = data.path().join("caller.cmd");
                // Search PATH using an ASCII command name: no CALL expansion of
                // the Unicode directory's literal percent signs is needed.
                fs::write(
                    &harness,
                    format!(
                        concat!(
                            "@echo off\r\n@setlocal EnableDelayedExpansion\r\n",
                            "@chcp {code_page} >nul\r\n@if errorlevel 1 exit /b 99\r\n",
                            "@call envnexus-unicode-fixture.cmd --exact terminal::tests::unicode_terminal_script_fixture --nocapture --skip \"%ENVNEXUS_TERMINAL_FIXTURE_LOG%\"\r\n",
                            "@set \"_fixture_exit=!errorlevel!\"\r\n",
                            "@echo WRAPPER_EXIT=!_fixture_exit!\r\n",
                            "@for /f \"tokens=2 delims=:\" %%C in ('chcp') do @echo RESTORED_CP=%%C\r\n",
                            "@exit /b 0\r\n"
                        ),
                        code_page = code_page,
                    ),
                )
                .unwrap();
                let output = crate::process::run_capture_with_env(
                    Path::new(&cmd),
                    &["/D", "/C", harness.to_str().unwrap()],
                    Duration::from_secs(20),
                    &[
                        ("PATH", OsStr::new(&path)),
                        ("ENVNEXUS_TERMINAL_FIXTURE_EXE", executable.as_os_str()),
                        ("ENVNEXUS_TERMINAL_FIXTURE_LOG", logfile.as_os_str()),
                        ("ENVNEXUS_TERMINAL_FIXTURE_FAIL", OsStr::new(fail)),
                        ("literal", OsStr::new("MUST_NOT_EXPAND")),
                    ],
                )
                .unwrap();
                let text = crate::process::output_text(&output);
                assert!(output.status.success(), "{text}");
                assert!(text.contains("UNICODE_TERMINAL_FIXTURE_OK"), "{text}");
                assert!(
                    text.contains(&format!("WRAPPER_EXIT={expected_exit}")),
                    "{text}"
                );
                assert!(
                    text.lines().any(|line| line
                        .strip_prefix("RESTORED_CP=")
                        .is_some_and(|value| value.trim() == code_page.to_string())),
                    "{text}"
                );
                assert!(
                    logfile.is_file(),
                    "the native argument path must reach the fixture"
                );
            }
        }
        assert_eq!(
            std::env::var_os("ENVNEXUS_TERMINAL_FIXTURE_EXE"),
            original_fixture
        );
    }

    #[cfg(windows)]
    #[test]
    fn powershell_invokes_the_same_unicode_command_and_preserves_failure_exit() {
        use std::{ffi::OsStr, time::Duration};

        let data = tempfile::tempdir().unwrap();
        let commands = data.path().join("中文-日本語-é PowerShell & !bang!");
        fs::create_dir_all(&commands).unwrap();
        let binaries = data.path().join("中文-日本語-é space & %literal% !bang!");
        fs::create_dir_all(&binaries).unwrap();
        let executable = binaries.join("可信 EnvNexus & %literal% !bang!.exe");
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let escaped = escape_batch_path(&executable).unwrap();
        fs::write(
            commands.join("envnexus-unicode-fixture.cmd"),
            utf8_command_script(&format!("@\"{escaped}\" %*\r\n")),
        )
        .unwrap();
        let logfile = data.path().join("中文-日本語-é PowerShell args &.log");
        let module_cache = data.path().join("powershell-module-cache");
        let path = format!(
            "{};{}",
            commands.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let powershell =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
                .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let output = crate::process::run_capture_with_env(
            &powershell,
            &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "& envnexus-unicode-fixture.cmd --exact terminal::tests::unicode_terminal_script_fixture --nocapture --skip $env:ENVNEXUS_TERMINAL_FIXTURE_LOG; exit $LASTEXITCODE"],
            Duration::from_secs(20),
            &[
                ("PATH", OsStr::new(&path)),
                ("ENVNEXUS_TERMINAL_FIXTURE_EXE", executable.as_os_str()),
                ("ENVNEXUS_TERMINAL_FIXTURE_LOG", logfile.as_os_str()),
                ("ENVNEXUS_TERMINAL_FIXTURE_FAIL", OsStr::new("1")),
                ("PSModuleAnalysisCachePath", module_cache.as_os_str()),
                ("literal", OsStr::new("MUST_NOT_EXPAND")),
            ],
        ).unwrap();
        let text = crate::process::output_text(&output);
        assert_eq!(output.status.code(), Some(23), "{text}");
        assert!(text.contains("UNICODE_TERMINAL_FIXTURE_OK"), "{text}");
    }
}
