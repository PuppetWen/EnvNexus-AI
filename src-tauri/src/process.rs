use std::{
    ffi::OsStr,
    io::{self, Read},
    path::Path,
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::error::{AppError, AppResult};

#[cfg(windows)]
use std::os::windows::{
    io::{AsHandle, AsRawHandle, OwnedHandle},
    process::CommandExt,
};

#[cfg(windows)]
trait CapturePipe: Read + Send + AsHandle {}
#[cfg(windows)]
impl<T: Read + Send + AsHandle> CapturePipe for T {}

#[cfg(not(windows))]
trait CapturePipe: Read + Send {}
#[cfg(not(windows))]
impl<T: Read + Send> CapturePipe for T {}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(1);
const READER_CANCEL_GRACE: Duration = Duration::from_millis(250);

struct OutputReader {
    thread: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    #[cfg(windows)]
    cancellation_pipe: OwnedHandle,
}

impl OutputReader {
    fn spawn(mut pipe: impl CapturePipe + 'static, name: &str) -> io::Result<Self> {
        #[cfg(windows)]
        let cancellation_pipe = pipe.as_handle().try_clone_to_owned()?;
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let mut captured = Vec::new();
                let mut buffer = [0_u8; 16 * 1024];
                while !reader_stop.load(Ordering::Acquire) {
                    match pipe.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            // Keep draining after the storage limit so a noisy child
                            // cannot block on a full pipe or grow memory indefinitely.
                            let keep = count.min(MAX_CAPTURE_BYTES - captured.len());
                            captured.extend_from_slice(&buffer[..keep]);
                        }
                        Err(_) if reader_stop.load(Ordering::Acquire) => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    }
                }
                Ok(captured)
            })?;
        Ok(Self {
            thread: Some(thread),
            stop,
            #[cfg(windows)]
            cancellation_pipe,
        })
    }

    fn cancel(&self) {
        self.stop.store(true, Ordering::Release);
        #[cfg(windows)]
        if let Some(thread) = &self.thread {
            // Rust's ChildStdout/ChildStderr use overlapped ReadFileEx operations.
            // The duplicate owns the same pipe object and stays valid even if the
            // reader finishes concurrently. CancelIoEx wakes its alertable wait;
            // thread cancellation also covers synchronous pipe implementations.
            unsafe {
                windows_sys::Win32::System::IO::CancelIoEx(
                    self.cancellation_pipe.as_raw_handle(),
                    std::ptr::null(),
                );
                windows_sys::Win32::System::IO::CancelSynchronousIo(thread.as_raw_handle());
            }
        }
    }

    fn finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }

    fn finish(mut self, deadline: Instant) -> io::Result<Vec<u8>> {
        while !self.finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if !self.finished() {
            let cancel_deadline = Instant::now() + READER_CANCEL_GRACE;
            // Repeat cancellation to cover a read beginning just after the stop
            // check. A descendant may still own the write end of either pipe.
            while !self.finished() && Instant::now() < cancel_deadline {
                self.cancel();
                thread::sleep(Duration::from_millis(5));
            }
        }
        if !self.finished() {
            // Windows cancellation is best effort. Do not block the caller on an
            // unresponsive I/O operation; dropping JoinHandle detaches the reader.
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "子进程输出管道未能在期限内关闭",
            ));
        }
        self.thread
            .take()
            .expect("output reader owns its thread")
            .join()
            .map_err(|_| io::Error::other("读取子进程输出的线程异常退出"))?
    }
}

impl Drop for OutputReader {
    fn drop(&mut self) {
        self.cancel();
    }
}

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
    let readers = (|| -> io::Result<_> {
        let stdout = OutputReader::spawn(
            child.stdout.take().expect("stdout was configured as piped"),
            "envnexus-stdout",
        )?;
        let stderr = OutputReader::spawn(
            child.stderr.take().expect("stderr was configured as piped"),
            "envnexus-stderr",
        )?;
        Ok((stdout, stderr))
    })();
    let (stdout, stderr) = match readers {
        Ok(readers) => readers,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::from(error));
        }
    };

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => break Err(AppError::from(error)),
        }
        if started.elapsed() >= timeout {
            break Err(AppError::Message(format!(
                "命令执行超时：{}",
                program.to_string_lossy()
            )));
        }
        thread::sleep(Duration::from_millis(25));
    };
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        stdout.cancel();
        stderr.cancel();
    }
    let deadline = Instant::now()
        + if status.is_ok() {
            OUTPUT_DRAIN_GRACE
        } else {
            Duration::ZERO
        };
    let captured_stdout = stdout.finish(deadline);
    let captured_stderr = stderr.finish(deadline);
    let output = Output {
        status: status?,
        stdout: captured_stdout?,
        stderr: captured_stderr?,
    };
    #[cfg(windows)]
    if let Some(reason) = native_startup_failure(&output) {
        return Err(AppError::Message(format!(
            "无法启动 {}：{reason}（Windows 错误 0x{:08X}）",
            program.to_string_lossy(),
            output.status.code().unwrap_or_default() as u32,
        )));
    }
    Ok(output)
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

    const CAPTURE_FIXTURE_ENV: &str = "ENVNEXUS_CAPTURE_TEST_MODE";
    const CAPTURE_FIXTURE_ARGS: &[&str] = &[
        "--exact",
        "process::tests::capture_tool_fixture",
        "--nocapture",
    ];

    #[test]
    // The parent regression owns and terminates this fixture's reported child.
    #[allow(clippy::zombie_processes)]
    fn capture_tool_fixture() {
        match std::env::var(CAPTURE_FIXTURE_ENV).as_deref() {
            Ok("large") => {
                fn write_output(mut writer: impl io::Write, byte: u8) {
                    let buffer = [byte; 64 * 1024];
                    for _ in 0..MAX_CAPTURE_BYTES / buffer.len() + 2 {
                        writer.write_all(&buffer).unwrap();
                    }
                    writer.flush().unwrap();
                }
                thread::scope(|scope| {
                    scope.spawn(|| write_output(io::stdout().lock(), b'O'));
                    scope.spawn(|| write_output(io::stderr().lock(), b'E'));
                });
            }
            Ok("spawn-holder") => {
                let holder = Command::new(std::env::current_exe().unwrap())
                    .args(CAPTURE_FIXTURE_ARGS)
                    .env(CAPTURE_FIXTURE_ENV, "sleep")
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .unwrap();
                println!("PIPE_HOLDER_PID={}", holder.id());
                eprintln!("PIPE_HOLDER_STDERR_READY");
            }
            Ok("sleep") => thread::sleep(Duration::from_secs(30)),
            _ => {}
        }
    }

    #[test]
    fn large_stdout_and_stderr_are_drained_while_capture_is_capped() {
        let output = run_capture_with_env(
            &std::env::current_exe().unwrap(),
            CAPTURE_FIXTURE_ARGS,
            Duration::from_secs(20),
            &[(CAPTURE_FIXTURE_ENV, OsStr::new("large"))],
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), MAX_CAPTURE_BYTES);
        assert_eq!(output.stderr.len(), MAX_CAPTURE_BYTES);
        assert!(
            output.stdout.iter().filter(|byte| **byte == b'O').count() > MAX_CAPTURE_BYTES - 4096
        );
        assert!(output.stderr.iter().all(|byte| *byte == b'E'));
    }

    #[test]
    fn inherited_output_pipes_have_a_bounded_drain_period() {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
        };

        let started = Instant::now();
        let output = run_capture_with_env(
            &std::env::current_exe().unwrap(),
            CAPTURE_FIXTURE_ARGS,
            Duration::from_secs(10),
            &[(CAPTURE_FIXTURE_ENV, OsStr::new("spawn-holder"))],
        )
        .unwrap();
        let elapsed = started.elapsed();
        let text = output_text(&output);
        let pid = text
            .lines()
            .find_map(|line| line.split_once("PIPE_HOLDER_PID="))
            .and_then(|(_, value)| value.trim().parse::<u32>().ok())
            .expect("fixture must identify its pipe-holding child");
        // Stop only the trusted fixture PID so this regression leaves no sleeper.
        let terminated = unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            assert!(!handle.is_null());
            let result = TerminateProcess(handle, 0);
            CloseHandle(handle);
            result
        };
        assert_ne!(terminated, 0);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("PIPE_HOLDER_STDERR_READY"));
        assert!(elapsed < Duration::from_secs(6), "capture took {elapsed:?}");
    }

    #[test]
    fn capture_timeout_does_not_wait_for_output_readers() {
        let started = Instant::now();
        let error = run_capture_with_env(
            &std::env::current_exe().unwrap(),
            CAPTURE_FIXTURE_ARGS,
            Duration::from_millis(150),
            &[(CAPTURE_FIXTURE_ENV, OsStr::new("sleep"))],
        )
        .unwrap_err();
        assert!(error.to_string().contains("命令执行超时"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    fn pe_u16(bytes: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
        ))
    }

    fn pe_u32(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
        ))
    }

    fn startup_import_names(executable: &[u8]) -> Option<Vec<std::ops::Range<usize>>> {
        if executable.get(..2)? != b"MZ" {
            return None;
        }
        let pe = usize::try_from(pe_u32(executable, 0x3c)?).ok()?;
        if executable.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
            return None;
        }
        let coff = pe.checked_add(4)?;
        let section_count = usize::from(pe_u16(executable, coff.checked_add(2)?)?);
        let optional_size = usize::from(pe_u16(executable, coff.checked_add(16)?)?);
        let optional_start = coff.checked_add(20)?;
        let sections_start = optional_start.checked_add(optional_size)?;
        let optional = executable.get(optional_start..sections_start)?;
        let directories = match pe_u16(optional, 0)? {
            0x10b => 96,  // PE32
            0x20b => 112, // PE32+
            _ => return None,
        };
        if pe_u32(optional, directories - 4)? < 2 {
            return None;
        }
        let imports_rva = usize::try_from(pe_u32(optional, directories + 8)?).ok()?;
        let imports_size = usize::try_from(pe_u32(optional, directories + 12)?).ok()?;
        let headers_size = usize::try_from(pe_u32(optional, 60)?).ok()?;
        let sections = executable
            .get(sections_start..sections_start.checked_add(section_count.checked_mul(40)?)?)?;
        let rva_to_offset = |rva: usize, size: usize| -> Option<usize> {
            if rva.checked_add(size)? <= headers_size {
                executable.get(rva..rva.checked_add(size)?)?;
                return Some(rva);
            }
            for section in sections.as_chunks::<40>().0 {
                let virtual_address = usize::try_from(pe_u32(section, 12)?).ok()?;
                let raw_size = usize::try_from(pe_u32(section, 16)?).ok()?;
                let raw_offset = usize::try_from(pe_u32(section, 20)?).ok()?;
                if let Some(relative) = rva.checked_sub(virtual_address)
                    && relative.checked_add(size)? <= raw_size
                {
                    let offset = raw_offset.checked_add(relative)?;
                    executable.get(offset..offset.checked_add(size)?)?;
                    return Some(offset);
                }
            }
            None
        };
        if imports_rva == 0 {
            return None;
        }
        let mut names = Vec::new();
        for index in 0..imports_size / 20 {
            let descriptor_rva = imports_rva.checked_add(index.checked_mul(20)?)?;
            let offset = rva_to_offset(descriptor_rva, 20)?;
            let descriptor = executable.get(offset..offset.checked_add(20)?)?;
            if descriptor.iter().all(|byte| *byte == 0) {
                return Some(names);
            }
            let name_rva = usize::try_from(pe_u32(descriptor, 12)?).ok()?;
            let name_offset = rva_to_offset(name_rva, 1)?;
            let length = executable
                .get(name_offset..)?
                .iter()
                .take(256)
                .position(|byte| *byte == 0)?;
            if length == 0 || rva_to_offset(name_rva, length.checked_add(1)?)? != name_offset {
                return None;
            }
            names.push(name_offset..name_offset.checked_add(length)?);
        }
        None
    }

    #[test]
    fn startup_import_parser_follows_pe_sections_and_ignores_string_literals() {
        for (magic, optional_size, directories) in [(0x10b_u16, 224, 96), (0x20b, 240, 112)] {
            let mut executable = vec![0_u8; 0x600];
            executable[..2].copy_from_slice(b"MZ");
            executable[0x3c..0x40].copy_from_slice(&0x80_u32.to_le_bytes());
            executable[0x80..0x84].copy_from_slice(b"PE\0\0");
            executable[0x86..0x88].copy_from_slice(&1_u16.to_le_bytes());
            executable[0x94..0x96].copy_from_slice(&(optional_size as u16).to_le_bytes());
            let optional = 0x98;
            executable[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
            executable[optional + 60..optional + 64].copy_from_slice(&0x200_u32.to_le_bytes());
            executable[optional + directories - 4..optional + directories]
                .copy_from_slice(&2_u32.to_le_bytes());
            executable[optional + directories + 8..optional + directories + 12]
                .copy_from_slice(&0x1000_u32.to_le_bytes());
            executable[optional + directories + 12..optional + directories + 16]
                .copy_from_slice(&40_u32.to_le_bytes());
            let section = optional + optional_size;
            executable[section + 12..section + 16].copy_from_slice(&0x1000_u32.to_le_bytes());
            executable[section + 16..section + 20].copy_from_slice(&0x400_u32.to_le_bytes());
            executable[section + 20..section + 24].copy_from_slice(&0x200_u32.to_le_bytes());
            executable[0x20c..0x210].copy_from_slice(&0x1040_u32.to_le_bytes());
            executable[0x240..0x24d].copy_from_slice(b"kernel32.dll\0");
            executable[0x50..0x5d].copy_from_slice(b"kernel32.dll\0");

            let names = startup_import_names(&executable).unwrap();
            assert_eq!(names.len(), 1);
            assert_eq!(names[0], 0x240..0x24c);
            assert!(startup_import_names(&executable[..0x200]).is_none());
            executable[0x20c..0x210].copy_from_slice(&0x1400_u32.to_le_bytes());
            assert!(startup_import_names(&executable).is_none());
        }
    }

    #[test]
    fn startup_fixture_guard() {}

    #[test]
    fn missing_native_dependency_reports_error_without_a_loader_dialog() {
        // Use our own test executable as a trusted PE fixture. Replacing a startup
        // import with an absent DLL prevents it from ever reaching its entry point.
        let mut executable = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let import = startup_import_names(&executable)
            .expect("test executable must have a valid PE import directory")
            .into_iter()
            .filter(|name| {
                name.len() >= 12 && executable[name.end - 4..name.end].eq_ignore_ascii_case(b".dll")
            })
            .max_by_key(|name| name.len())
            .expect("test executable must have a startup DLL import");
        let random_stem = format!("NX{}", uuid::Uuid::new_v4().simple());
        let mut missing = (0..import.len() - 4)
            .map(|index| random_stem.as_bytes()[index % random_stem.len()])
            .collect::<Vec<_>>();
        missing.extend_from_slice(b".dll");
        assert!(!executable[import.clone()].eq_ignore_ascii_case(&missing));
        executable[import].copy_from_slice(&missing);
        let directory = tempfile::tempdir().unwrap();
        let fixture = directory.path().join("missing-dependency.exe");
        std::fs::write(&fixture, executable).unwrap();

        // Even if a future loader ignores the patched import, this child can only
        // execute the empty guard, never recursively run the entire test suite.
        let error = run_capture(
            &fixture,
            &["--exact", "process::tests::startup_fixture_guard"],
            Duration::from_secs(5),
        )
        .unwrap_err();
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
