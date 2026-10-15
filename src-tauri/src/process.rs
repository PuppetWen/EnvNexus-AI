use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::error::{AppError, AppResult};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn initialize_error_mode() {
    #[cfg(windows)]
    {
        use std::sync::Once;
        use windows_sys::Win32::System::Diagnostics::Debug::{
            GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX,
            SetErrorMode,
        };

        static INITIALIZED: Once = Once::new();
        INITIALIZED.call_once(|| unsafe {
            // Windows children inherit the process error mode, not a thread override.
            // Set this once, preserving inherited flags, so concurrent probes never
            // temporarily restore a mode that can display a native loader dialog.
            SetErrorMode(
                GetErrorMode()
                    | SEM_FAILCRITICALERRORS
                    | SEM_NOGPFAULTERRORBOX
                    | SEM_NOOPENFILEERRORBOX,
            );
        });
    }
}

pub fn run_capture(program: &Path, args: &[&str], timeout: Duration) -> AppResult<Output> {
    run_capture_with_env(program, args, timeout, &[])
}

pub fn run_capture_with_env(
    program: &Path,
    args: &[&str],
    timeout: Duration,
    environment: &[(&str, &OsStr)],
) -> AppResult<Output> {
    initialize_error_mode();
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(environment.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command.spawn().map_err(|error| {
        AppError::Message(format!("无法执行 {}：{error}", program.to_string_lossy()))
    })?;
    let started = Instant::now();

    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            #[cfg(windows)]
            if let Some(reason) = native_startup_failure(&output) {
                return Err(AppError::Message(format!(
                    "无法启动 {}：{reason}（Windows 错误 0x{:08X}）",
                    program.to_string_lossy(),
                    output.status.code().unwrap_or_default() as u32,
                )));
            }
            return Ok(output);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::Message(format!(
                "命令执行超时：{}",
                program.to_string_lossy()
            )));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn native_startup_failure(output: &Output) -> Option<&'static str> {
    match output.status.code()? as u32 {
        0xC000_0135 => Some("缺少所需 DLL，请重新安装该工具的完整官方发行包"),
        0xC000_0142 => Some("DLL 初始化失败，请检查该工具的完整安装"),
        0xC000_007B => Some("程序或 DLL 架构不兼容，或安装文件已损坏"),
        0xC000_0139 => Some("DLL 缺少所需入口点，请检查该工具的版本是否匹配"),
        _ => None,
    }
}

pub fn output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}\n{stderr}").trim().to_string()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn missing_native_dependency_reports_error_without_a_loader_dialog() {
        // Use our own test executable as a trusted PE fixture. Replacing a startup
        // import with an absent DLL prevents it from ever reaching its entry point.
        let mut executable = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let original = b"kernel32.dll";
        let missing = b"NX_TEST_.dll";
        let mut replacements = 0;
        for offset in 0..executable.len().saturating_sub(original.len()) {
            if executable[offset..offset + original.len()].eq_ignore_ascii_case(original)
                && executable[offset + original.len()] == 0
            {
                executable[offset..offset + original.len()].copy_from_slice(missing);
                replacements += 1;
            }
        }
        assert!(replacements > 0, "test executable must import kernel32.dll");
        let directory = tempfile::tempdir().unwrap();
        let fixture = directory.path().join("missing-dependency.exe");
        std::fs::write(&fixture, executable).unwrap();

        let error = run_capture(&fixture, &[], Duration::from_secs(5)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("缺少所需 DLL"), "{message}");
        assert!(message.contains("0xC0000135"), "{message}");
        assert!(message.contains("missing-dependency.exe"), "{message}");
    }

    #[test]
    fn malformed_executable_returns_a_spawn_error() {
        let directory = tempfile::tempdir().unwrap();
        let fixture = directory.path().join("broken-tool.exe");
        std::fs::write(&fixture, b"not a Windows executable").unwrap();
        let error = run_capture(&fixture, &[], Duration::from_secs(5)).unwrap_err();
        assert!(error.to_string().contains("无法执行"));
        assert!(error.to_string().contains("broken-tool.exe"));
    }

    #[test]
    fn command_environment_is_applied_only_to_child() {
        let key = "ENVNEXUS_INSTALL_PROBE_TEST";
        let original = std::env::var_os(key);
        let program = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let output = run_capture_with_env(
            Path::new(&program),
            &["/D", "/C", "echo %ENVNEXUS_INSTALL_PROBE_TEST%"],
            Duration::from_secs(5),
            &[(key, OsStr::new("isolated-probe"))],
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output_text(&output), "isolated-probe");
        assert_eq!(std::env::var_os(key), original);
    }
}
