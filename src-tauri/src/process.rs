use std::{
    io::Read,
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

pub fn run_capture(program: &Path, args: &[&str], timeout: Duration) -> AppResult<Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command.spawn().map_err(|error| {
        AppError::Message(format!("无法执行 {}：{error}", program.to_string_lossy()))
    })?;
    let started = Instant::now();

    // 边等待边在后台线程排空管道；输出超过管道缓冲的子进程
    // 否则会阻塞在写入端，直到超时被误杀。
    let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
    let stderr_reader = child.stderr.take().map(spawn_pipe_reader);
    let collect = |reader: Option<thread::JoinHandle<Vec<u8>>>| {
        reader
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default()
    };

    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Output {
                status,
                stdout: collect(stdout_reader),
                stderr: collect(stderr_reader),
            });
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            // 子进程结束后管道写入端关闭，读取线程会自行退出。
            let _ = collect(stdout_reader);
            let _ = collect(stderr_reader);
            return Err(AppError::Message(format!(
                "命令执行超时：{}",
                program.to_string_lossy()
            )));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_pipe_reader<R: Read + Send + 'static>(mut pipe: R) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        buffer
    })
}

pub fn output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}\n{stderr}").trim().to_string()
}
