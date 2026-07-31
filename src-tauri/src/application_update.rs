use std::{
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use minisign_verify::{PublicKey, Signature};
use reqwest::{
    StatusCode,
    header::{self, HeaderMap},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tokio::{
    fs::{File as AsyncFile, OpenOptions},
    io::AsyncWriteExt,
};
use url::Url;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};
#[cfg(windows)]
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

const UPDATE_PUBLIC_KEY: &str = "RWR64ppt8nqmQSpZTij7HZJOI0w2uhUJdOCIRkNJV2I8qDzznLaBZUyK";
const UPDATE_HELPER_ARGUMENT: &str = "--envnexus-update-helper";
const UPDATE_JOURNAL_ENV: &str = "ENVNEXUS_UPDATE_JOURNAL";
const DOWNLOAD_MAX_ATTEMPTS: u32 = 8;
const HELPER_WAIT_TIMEOUT: Duration = Duration::from_secs(90);
const READY_WAIT_TIMEOUT: Duration = Duration::from_secs(75);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ApplicationInstallKind {
    Installed,
    Portable,
}

impl ApplicationInstallKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Portable => "portable",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationUpdateAsset {
    pub url: String,
    pub signature: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareApplicationUpdateRequest {
    pub version: String,
    pub installer: ApplicationUpdateAsset,
    pub portable: ApplicationUpdateAsset,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedApplicationUpdate {
    pub operation_id: String,
    pub version: String,
    pub install_kind: ApplicationInstallKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplicationUpdateProgress {
    operation_id: String,
    phase: String,
    message: String,
    received_bytes: u64,
    total_bytes: Option<u64>,
    percent: Option<f64>,
    attempt: u32,
    max_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateJournal {
    schema_version: u32,
    operation_id: String,
    version: String,
    install_kind: ApplicationInstallKind,
    state: String,
    parent_pid: u32,
    target_executable: PathBuf,
    payload_path: PathBuf,
    payload_sha256: String,
    target_sha256: String,
    helper_path: PathBuf,
    backup_path: PathBuf,
    replacement_path: PathBuf,
    ready_marker: PathBuf,
    log_path: PathBuf,
    error: Option<String>,
}

pub fn detect_install_kind() -> ApplicationInstallKind {
    std::env::current_exe()
        .ok()
        .map(|path| detect_install_kind_for(&path))
        .unwrap_or(ApplicationInstallKind::Portable)
}

#[cfg(windows)]
fn detect_install_kind_for(executable: &Path) -> ApplicationInstallKind {
    let Some(parent) = executable.parent() else {
        return ApplicationInstallKind::Portable;
    };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(product) = hkcu.open_subkey("Software\\envpilot\\EnvNexus AI") else {
        return ApplicationInstallKind::Portable;
    };
    let Ok(value) = product.get_value::<String, _>("") else {
        return ApplicationInstallKind::Portable;
    };
    let installed = PathBuf::from(value.trim().trim_matches('"'));
    if paths_equal_case_insensitive(parent, &installed) {
        ApplicationInstallKind::Installed
    } else {
        ApplicationInstallKind::Portable
    }
}

#[cfg(not(windows))]
fn detect_install_kind_for(_executable: &Path) -> ApplicationInstallKind {
    ApplicationInstallKind::Portable
}

fn paths_equal_case_insensitive(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        path.components()
            .collect::<PathBuf>()
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_ascii_lowercase()
    };
    normalize(left) == normalize(right)
}

pub async fn prepare(
    client: &reqwest::Client,
    data_root: &Path,
    request: PrepareApplicationUpdateRequest,
    app: &AppHandle,
) -> AppResult<PreparedApplicationUpdate> {
    validate_version(&request.version)?;
    validate_asset(&request.installer)?;
    validate_asset(&request.portable)?;

    let install_kind = detect_install_kind();
    let selected_asset = match install_kind {
        ApplicationInstallKind::Installed => &request.installer,
        ApplicationInstallKind::Portable => &request.portable,
    };
    let operation_id = Uuid::new_v4().to_string();
    let update_root = data_root.join("updates").join(&operation_id);
    tokio::fs::create_dir_all(&update_root).await?;

    emit_progress(
        Some(app),
        &operation_id,
        "preparing",
        match install_kind {
            ApplicationInstallKind::Installed => "正在准备安装版静默更新",
            ApplicationInstallKind::Portable => "正在准备便携版原路径更新",
        },
        0,
        None,
        1,
    );

    let payload_path = match download_and_verify(
        client,
        &update_root,
        selected_asset,
        &operation_id,
        Some(app),
    )
    .await
    {
        Ok(path) => path,
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&update_root).await;
            return Err(error);
        }
    };

    let current_executable = std::env::current_exe()?;
    let target_name = current_executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::Message("当前主程序文件名无效".to_string()))?;
    let target_parent = current_executable
        .parent()
        .ok_or_else(|| AppError::Message("当前主程序缺少父目录".to_string()))?;
    ensure_directory_writable(target_parent)?;

    let helper_path = update_root.join("envnexus-update-helper.exe");
    tokio::fs::copy(&current_executable, &helper_path).await?;
    let backup_path = target_parent.join(format!(".{target_name}.envnexus-backup"));
    let replacement_path = target_parent.join(format!(".{target_name}.envnexus-new"));
    let ready_marker = update_root.join("new-version.ready");
    let log_path = update_root.join("update.log");
    for stale in [&backup_path, &replacement_path, &ready_marker] {
        if stale.exists() {
            let _ = tokio::fs::remove_file(stale).await;
        }
    }

    let journal = UpdateJournal {
        schema_version: 1,
        operation_id: operation_id.clone(),
        version: request.version.clone(),
        install_kind,
        state: "prepared".to_string(),
        parent_pid: std::process::id(),
        target_executable: current_executable,
        payload_path,
        payload_sha256: selected_asset.sha256.to_ascii_lowercase(),
        target_sha256: request.portable.sha256.to_ascii_lowercase(),
        helper_path,
        backup_path,
        replacement_path,
        ready_marker,
        log_path,
        error: None,
    };
    write_journal(&update_root.join("journal.json"), &journal)?;
    emit_progress(
        Some(app),
        &operation_id,
        "ready",
        "更新包校验通过，已准备安全替换和失败回滚",
        1,
        Some(1),
        1,
    );

    Ok(PreparedApplicationUpdate {
        operation_id,
        version: request.version,
        install_kind,
        message: "更新包已校验，程序即将重启以完成更新".to_string(),
    })
}

pub fn launch(data_root: &Path, operation_id: &str) -> AppResult<()> {
    validate_operation_id(operation_id)?;
    let journal_path = data_root
        .join("updates")
        .join(operation_id)
        .join("journal.json");
    let journal = read_journal(&journal_path)?;
    validate_journal(&journal_path, &journal)?;
    if journal.state != "prepared" {
        return Err(AppError::Message("更新事务不处于可启动状态".to_string()));
    }

    let mut command = Command::new(&journal.helper_path);
    command
        .arg(UPDATE_HELPER_ARGUMENT)
        .arg(&journal_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.spawn()?;
    Ok(())
}

pub fn helper_exit_code_from_args() -> Option<ExitCode> {
    let mut args = std::env::args_os();
    let _executable = args.next()?;
    if args.next()?.to_string_lossy() != UPDATE_HELPER_ARGUMENT {
        return None;
    }
    let Some(journal_path) = args.next().map(PathBuf::from) else {
        return Some(ExitCode::from(2));
    };
    Some(match run_helper(&journal_path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = append_log(
                &journal_path.with_file_name("update.log"),
                &format!("helper_error={error}"),
            );
            ExitCode::from(1)
        }
    })
}

fn run_helper(journal_path: &Path) -> AppResult<()> {
    let mut journal = read_journal(journal_path)?;
    validate_journal(journal_path, &journal)?;
    let _ = append_log(&journal.log_path, "helper_started");
    wait_for_process_exit(journal.parent_pid, HELPER_WAIT_TIMEOUT)?;
    let _ = append_log(&journal.log_path, "parent_exited");

    let backup_result = (|| -> AppResult<()> {
        if journal.backup_path.exists() {
            fs::remove_file(&journal.backup_path)?;
        }
        fs::copy(&journal.target_executable, &journal.backup_path)?;
        journal.state = "backup_created".to_string();
        write_journal(journal_path, &journal)
    })();
    if let Err(error) = backup_result {
        let _ = append_log(&journal.log_path, &format!("backup_failed={error}"));
        let _ = launch_target(&journal.target_executable, None);
        return Err(error);
    }

    let result = apply_update_and_wait_for_ready(journal_path, &mut journal);
    if let Err(error) = result {
        let _ = append_log(&journal.log_path, &format!("update_failed={error}"));
        journal.error = Some(error.to_string());
        journal.state = "rolling_back".to_string();
        let _ = write_journal(journal_path, &journal);
        restore_backup(&journal)?;
        let _child = launch_target(&journal.target_executable, None)?;
        journal.state = "rolled_back".to_string();
        let _ = write_journal(journal_path, &journal);
        let _ = append_log(&journal.log_path, "rollback_completed");
        let _ = fs::remove_file(&journal.payload_path);
        return Err(error);
    }

    journal.state = "committed".to_string();
    write_journal(journal_path, &journal)?;
    let _ = append_log(&journal.log_path, "update_committed");
    let _ = fs::remove_file(&journal.backup_path);
    let _ = fs::remove_file(&journal.payload_path);
    Ok(())
}

fn apply_update_and_wait_for_ready(
    journal_path: &Path,
    journal: &mut UpdateJournal,
) -> AppResult<()> {
    match journal.install_kind {
        ApplicationInstallKind::Portable => apply_portable_update(journal)?,
        ApplicationInstallKind::Installed => apply_installed_update(journal)?,
    }
    journal.state = "replacement_applied".to_string();
    write_journal(journal_path, journal)?;

    if !hash_file_sha256(&journal.target_executable)?.eq_ignore_ascii_case(&journal.target_sha256) {
        return Err(AppError::Message(
            "替换后的主程序校验失败，拒绝提交更新".to_string(),
        ));
    }

    let mut child = launch_target(&journal.target_executable, Some(journal_path))?;
    journal.state = "waiting_for_new_version".to_string();
    write_journal(journal_path, journal)?;
    let started = Instant::now();
    while started.elapsed() < READY_WAIT_TIMEOUT {
        if journal.ready_marker.is_file() {
            let marker = fs::read_to_string(&journal.ready_marker).unwrap_or_default();
            if marker.trim() == journal.operation_id {
                let _ = child.try_wait();
                return Ok(());
            }
        }
        if child.try_wait()?.is_some() {
            return Err(AppError::Message("新版本在确认启动前已退出".to_string()));
        }
        thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(AppError::Message(
        "新版本未在限定时间内完成启动确认".to_string(),
    ))
}

fn apply_portable_update(journal: &UpdateJournal) -> AppResult<()> {
    if journal.replacement_path.exists() {
        fs::remove_file(&journal.replacement_path)?;
    }
    fs::copy(&journal.payload_path, &journal.replacement_path)?;
    if !hash_file_sha256(&journal.replacement_path)?.eq_ignore_ascii_case(&journal.target_sha256) {
        let _ = fs::remove_file(&journal.replacement_path);
        return Err(AppError::Message("便携版替换文件二次校验失败".to_string()));
    }
    fs::remove_file(&journal.target_executable)?;
    fs::rename(&journal.replacement_path, &journal.target_executable)?;
    Ok(())
}

fn apply_installed_update(journal: &UpdateJournal) -> AppResult<()> {
    let mut command = Command::new(&journal.payload_path);
    command
        .args(["/S", "/UPDATE", "/NS"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let status = command.status()?;
    if !status.success() {
        return Err(AppError::Message(format!(
            "静默安装程序返回失败状态：{}",
            status.code().unwrap_or(-1)
        )));
    }
    if !journal.target_executable.is_file() {
        return Err(AppError::Message(
            "安装程序完成后未找到新主程序".to_string(),
        ));
    }
    Ok(())
}

fn restore_backup(journal: &UpdateJournal) -> AppResult<()> {
    if !journal.backup_path.is_file() {
        return Err(AppError::Message(
            "更新失败且旧版本备份不存在，无法自动回滚".to_string(),
        ));
    }
    if journal.target_executable.exists() {
        let failed = journal
            .payload_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("failed-new-version.exe");
        let _ = fs::remove_file(&failed);
        if fs::rename(&journal.target_executable, &failed).is_err() {
            fs::remove_file(&journal.target_executable)?;
        }
    }
    fs::rename(&journal.backup_path, &journal.target_executable)?;
    Ok(())
}

fn launch_target(executable: &Path, journal_path: Option<&Path>) -> AppResult<std::process::Child> {
    let mut command = Command::new(executable);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(journal_path) = journal_path {
        command.env(UPDATE_JOURNAL_ENV, journal_path);
    }
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    Ok(command.spawn()?)
}

pub fn confirm_new_version_started() -> AppResult<bool> {
    let Some(journal_path) = std::env::var_os(UPDATE_JOURNAL_ENV).map(PathBuf::from) else {
        return Ok(false);
    };
    let journal = read_journal(&journal_path)?;
    validate_journal(&journal_path, &journal)?;
    if journal.version != env!("CARGO_PKG_VERSION") {
        return Err(AppError::Message(
            "更新启动确认中的版本号与当前程序不一致".to_string(),
        ));
    }
    if !paths_equal_case_insensitive(&std::env::current_exe()?, &journal.target_executable) {
        return Err(AppError::Message(
            "更新启动确认中的主程序路径不一致".to_string(),
        ));
    }
    fs::write(&journal.ready_marker, &journal.operation_id)?;
    schedule_committed_cleanup(journal_path);
    Ok(true)
}

fn schedule_committed_cleanup(journal_path: PathBuf) {
    thread::spawn(move || {
        for _ in 0..60 {
            thread::sleep(Duration::from_millis(500));
            let Ok(journal) = read_journal(&journal_path) else {
                continue;
            };
            if journal.state == "committed" {
                let update_root = journal_path.parent().map(Path::to_path_buf);
                if let Some(update_root) = update_root {
                    for _ in 0..20 {
                        if fs::remove_dir_all(&update_root).is_ok() || !update_root.exists() {
                            return;
                        }
                        thread::sleep(Duration::from_millis(500));
                    }
                }
                return;
            }
            if journal.state == "rolled_back" {
                return;
            }
        }
    });
}

pub fn cleanup_stale_updates(data_root: &Path) {
    let updates = data_root.join("updates");
    let Ok(entries) = fs::read_dir(&updates) else {
        cleanup_legacy_updater_temp();
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let journal_path = path.join("journal.json");
        let Ok(journal) = read_journal(&journal_path) else {
            continue;
        };
        if matches!(journal.state.as_str(), "committed" | "rolled_back") {
            let _ = fs::remove_file(&journal.backup_path);
            let _ = fs::remove_file(&journal.replacement_path);
            let _ = fs::remove_dir_all(path);
        }
    }
    cleanup_legacy_updater_temp();
}

fn cleanup_legacy_updater_temp() {
    let temp = std::env::temp_dir();
    let Ok(entries) = fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.starts_with("envnexus ai-") && name.contains("-updater-") {
            let old_enough = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(300));
            if old_enough {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

async fn download_and_verify(
    client: &reqwest::Client,
    update_root: &Path,
    asset: &ApplicationUpdateAsset,
    operation_id: &str,
    app: Option<&AppHandle>,
) -> AppResult<PathBuf> {
    let completed = update_root.join("update-payload.exe");
    let partial = update_root.join("update-payload.exe.part");
    if completed.is_file()
        && hash_file_sha256(&completed)?.eq_ignore_ascii_case(&asset.sha256)
        && verify_minisign(&completed, &asset.signature).is_ok()
    {
        return Ok(completed);
    }
    let _ = tokio::fs::remove_file(&completed).await;

    let mut received = 0;
    let mut total = None;
    for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
        let existing = tokio::fs::metadata(&partial)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        received = existing;
        let mut builder = client
            .get(&asset.url)
            .header(header::ACCEPT_ENCODING, "identity");
        if existing > 0 {
            builder = builder.header(header::RANGE, format!("bytes={existing}-"));
        }
        let response = match builder
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
        {
            Ok(response) => response,
            Err(error) if attempt < DOWNLOAD_MAX_ATTEMPTS && retryable_download_error(&error) => {
                emit_progress(
                    app,
                    operation_id,
                    "retrying",
                    &format!(
                        "网络中断，正在自动续传（第 {}/{DOWNLOAD_MAX_ATTEMPTS} 次）",
                        attempt + 1
                    ),
                    existing,
                    total,
                    attempt + 1,
                );
                tokio::time::sleep(retry_delay(attempt)).await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        validate_download_host(response.url())?;

        let resumed = existing > 0
            && response.status() == StatusCode::PARTIAL_CONTENT
            && content_range_starts_at(response.headers(), existing);
        if existing > 0 && response.status() == StatusCode::PARTIAL_CONTENT && !resumed {
            return Err(AppError::InvalidSource(
                "更新服务器返回了不匹配的续传范围，已拒绝拼接文件".to_string(),
            ));
        }
        let offset = if resumed { existing } else { 0 };
        let mut file = if resumed {
            OpenOptions::new().append(true).open(&partial).await?
        } else {
            AsyncFile::create(&partial).await?
        };
        total = response.content_length().map(|length| length + offset);
        received = offset;
        let mut stream = response.bytes_stream();
        let mut stream_error = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    file.write_all(&chunk).await?;
                    received += chunk.len() as u64;
                    emit_progress(
                        app,
                        operation_id,
                        "downloading",
                        if resumed {
                            "正在断点续传更新包"
                        } else {
                            "正在下载更新包"
                        },
                        received,
                        total,
                        attempt,
                    );
                }
                Err(error) => {
                    stream_error = Some(error);
                    break;
                }
            }
        }
        file.flush().await?;
        drop(file);

        if let Some(error) = stream_error {
            if attempt < DOWNLOAD_MAX_ATTEMPTS && retryable_download_error(&error) {
                emit_progress(
                    app,
                    operation_id,
                    "retrying",
                    &format!(
                        "下载连接已断开，正在从 {received} 字节处续传（第 {}/{DOWNLOAD_MAX_ATTEMPTS} 次）",
                        attempt + 1
                    ),
                    received,
                    total,
                    attempt + 1,
                );
                tokio::time::sleep(retry_delay(attempt)).await;
                continue;
            }
            return Err(error.into());
        }
        if total.is_some_and(|expected| received < expected) {
            if attempt < DOWNLOAD_MAX_ATTEMPTS {
                emit_progress(
                    app,
                    operation_id,
                    "retrying",
                    &format!(
                        "更新包提前结束，正在自动续传（第 {}/{DOWNLOAD_MAX_ATTEMPTS} 次）",
                        attempt + 1
                    ),
                    received,
                    total,
                    attempt + 1,
                );
                tokio::time::sleep(retry_delay(attempt)).await;
                continue;
            }
            return Err(AppError::Message(format!(
                "更新包下载在 {received} 字节处提前结束，自动续传次数已用尽"
            )));
        }
        break;
    }

    emit_progress(
        app,
        operation_id,
        "verifying",
        "正在校验 SHA-256 与发布签名",
        received,
        total,
        DOWNLOAD_MAX_ATTEMPTS,
    );
    let expected_hash = asset.sha256.clone();
    let signature = asset.signature.clone();
    let verify_path = partial.clone();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let actual = hash_file_sha256(&verify_path)?;
        if !actual.eq_ignore_ascii_case(&expected_hash) {
            return Err(AppError::ChecksumMismatch {
                expected: expected_hash,
                actual,
            });
        }
        verify_minisign(&verify_path, &signature)
    })
    .await
    .map_err(|error| AppError::Message(format!("更新校验任务异常结束：{error}")))??;
    tokio::fs::rename(&partial, &completed).await?;
    Ok(completed)
}

fn validate_asset(asset: &ApplicationUpdateAsset) -> AppResult<()> {
    let url = Url::parse(&asset.url)
        .map_err(|error| AppError::InvalidSource(format!("更新 URL 无效：{error}")))?;
    if url.scheme() != "https" {
        return Err(AppError::InvalidSource(
            "更新包必须使用 HTTPS 下载".to_string(),
        ));
    }
    validate_download_host(&url)?;
    if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::InvalidSource(
            "更新元数据中的 SHA-256 无效".to_string(),
        ));
    }
    decode_signature(&asset.signature)?;
    Ok(())
}

fn validate_download_host(url: &Url) -> AppResult<()> {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    #[cfg(test)]
    if matches!(host.as_str(), "127.0.0.1" | "localhost") {
        return Ok(());
    }
    if matches!(
        host.as_str(),
        "github.com" | "objects.githubusercontent.com" | "release-assets.githubusercontent.com"
    ) {
        Ok(())
    } else {
        Err(AppError::InvalidSource(format!(
            "更新下载重定向到了未授权主机：{host}"
        )))
    }
}

fn validate_version(version: &str) -> AppResult<()> {
    let valid = !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    if valid {
        Ok(())
    } else {
        Err(AppError::InvalidSource("更新版本号格式无效".to_string()))
    }
}

fn validate_operation_id(operation_id: &str) -> AppResult<()> {
    Uuid::parse_str(operation_id)
        .map(|_| ())
        .map_err(|_| AppError::InvalidSource("更新事务 ID 无效".to_string()))
}

fn validate_journal(journal_path: &Path, journal: &UpdateJournal) -> AppResult<()> {
    validate_operation_id(&journal.operation_id)?;
    if journal.schema_version != 1
        || journal_path.file_name().and_then(|name| name.to_str()) != Some("journal.json")
        || journal_path.parent() != journal.helper_path.parent()
        || journal_path.parent() != journal.payload_path.parent()
        || journal_path.parent() != journal.ready_marker.parent()
        || !journal.target_executable.is_absolute()
        || !journal.backup_path.is_absolute()
        || !journal.replacement_path.is_absolute()
    {
        return Err(AppError::InvalidSource(
            "更新事务清单包含不安全路径".to_string(),
        ));
    }
    Ok(())
}

fn verify_minisign(path: &Path, signature: &str) -> AppResult<()> {
    let public_key = PublicKey::from_base64(UPDATE_PUBLIC_KEY)
        .map_err(|error| AppError::Message(format!("内置更新公钥无效：{error}")))?;
    let signature = decode_signature(signature)?;
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|error| AppError::InvalidSource(format!("更新签名算法无效：{error}")))?;
    let mut reader = BufReader::new(File::open(path)?);
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier
        .finalize()
        .map_err(|error| AppError::Message(format!("更新包签名校验失败：{error}")))
}

fn decode_signature(value: &str) -> AppResult<Signature> {
    let value = value.trim();
    let decoded;
    let minisign_text = if value.starts_with("untrusted comment:") {
        value
    } else {
        decoded = STANDARD
            .decode(value)
            .map_err(|error| AppError::InvalidSource(format!("更新签名 Base64 无效：{error}")))?;
        std::str::from_utf8(&decoded).map_err(|error| {
            AppError::InvalidSource(format!("更新签名不是有效的 UTF-8 文本：{error}"))
        })?
    };
    Signature::decode(minisign_text)
        .map_err(|error| AppError::InvalidSource(format!("更新签名格式无效：{error}")))
}

fn hash_file_sha256(path: &Path) -> AppResult<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn ensure_directory_writable(directory: &Path) -> AppResult<()> {
    let probe = directory.join(format!(".envnexus-update-write-test-{}", Uuid::new_v4()));
    fs::write(&probe, b"write-test").map_err(|error| {
        AppError::Message(format!(
            "当前程序目录不可写，无法安全原地更新 {}：{error}",
            directory.display()
        ))
    })?;
    fs::remove_file(probe)?;
    Ok(())
}

fn read_journal(path: &Path) -> AppResult<UpdateJournal> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_journal(path: &Path, journal: &UpdateJournal) -> AppResult<()> {
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, journal)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn append_log(path: &Path, message: &str) -> AppResult<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{} {message}", chrono::Utc::now().to_rfc3339())?;
    file.flush()?;
    Ok(())
}

#[cfg(windows)]
fn wait_for_process_exit(pid: u32, timeout: Duration) -> AppResult<()> {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return Ok(());
    }
    let milliseconds = timeout.as_millis().min(u32::MAX as u128) as u32;
    let result = unsafe { WaitForSingleObject(handle, milliseconds) };
    unsafe {
        CloseHandle(handle);
    }
    match result {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(AppError::Message(
            "等待旧版本退出超时，未执行任何替换".to_string(),
        )),
        other => Err(AppError::Message(format!(
            "等待旧版本退出失败：Windows 状态码 {other}"
        ))),
    }
}

#[cfg(not(windows))]
fn wait_for_process_exit(_pid: u32, _timeout: Duration) -> AppResult<()> {
    Err(AppError::Message("当前平台尚不支持应用自更新".to_string()))
}

fn content_range_starts_at(headers: &HeaderMap, expected: u64) -> bool {
    headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("bytes "))
        .and_then(|value| value.split_once('-'))
        .and_then(|(start, _)| start.parse::<u64>().ok())
        == Some(expected)
}

fn retryable_download_error(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error.is_connect()
        || error.is_body()
        || error.is_decode()
        || error.is_request()
        || error.status().is_some_and(|status| {
            status.is_server_error()
                || status == StatusCode::REQUEST_TIMEOUT
                || status == StatusCode::TOO_MANY_REQUESTS
        })
}

fn retry_delay(failed_attempt: u32) -> Duration {
    Duration::from_millis(750 * 2_u64.pow(failed_attempt.saturating_sub(1).min(3)))
}

fn emit_progress(
    app: Option<&AppHandle>,
    operation_id: &str,
    phase: &str,
    message: &str,
    received_bytes: u64,
    total_bytes: Option<u64>,
    attempt: u32,
) {
    let Some(app) = app else {
        return;
    };
    let percent = total_bytes
        .filter(|total| *total > 0)
        .map(|total| (received_bytes as f64 / total as f64 * 100.0).clamp(0.0, 100.0));
    let _ = app.emit(
        "application-update-progress",
        ApplicationUpdateProgress {
            operation_id: operation_id.to_string(),
            phase: phase.to_string(),
            message: message.to_string(),
            received_bytes,
            total_bytes,
            percent,
            attempt,
            max_attempts: DOWNLOAD_MAX_ATTEMPTS,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn path_comparison_is_case_insensitive_and_ignores_trailing_separator() {
        assert!(paths_equal_case_insensitive(
            Path::new(r"C:\Users\Test\App"),
            Path::new(r"c:\users\test\app\\"),
        ));
    }

    #[test]
    fn rejects_untrusted_update_hosts_and_malformed_checksums() {
        let invalid_host = ApplicationUpdateAsset {
            url: "https://example.com/update.exe".to_string(),
            signature: "invalid".to_string(),
            sha256: "0".repeat(64),
        };
        assert!(validate_asset(&invalid_host).is_err());

        let invalid_hash = ApplicationUpdateAsset {
            url: "https://github.com/PuppetWen/EnvNexus-AI/releases/update.exe".to_string(),
            signature: "invalid".to_string(),
            sha256: "not-a-hash".to_string(),
        };
        assert!(validate_asset(&invalid_hash).is_err());
    }

    #[test]
    fn rollback_restores_the_previous_executable() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("envnexus-ai.exe");
        let backup = temp.path().join(".envnexus-ai.exe.envnexus-backup");
        fs::write(&target, b"new-version").unwrap();
        fs::write(&backup, b"old-version").unwrap();
        let journal = UpdateJournal {
            schema_version: 1,
            operation_id: Uuid::new_v4().to_string(),
            version: "0.1.3".to_string(),
            install_kind: ApplicationInstallKind::Portable,
            state: "rolling_back".to_string(),
            parent_pid: 1,
            target_executable: target.clone(),
            payload_path: temp.path().join("payload.exe"),
            payload_sha256: "0".repeat(64),
            target_sha256: "0".repeat(64),
            helper_path: temp.path().join("helper.exe"),
            backup_path: backup,
            replacement_path: temp.path().join("replacement.exe"),
            ready_marker: temp.path().join("ready"),
            log_path: temp.path().join("update.log"),
            error: None,
        };
        restore_backup(&journal).unwrap();
        assert_eq!(fs::read(target).unwrap(), b"old-version");
        assert_eq!(
            fs::read(temp.path().join("failed-new-version.exe")).unwrap(),
            b"new-version"
        );
    }

    #[test]
    #[ignore = "requires release assets generated by scripts/Prepare-Release.ps1"]
    fn built_release_assets_match_signed_metadata() {
        let release_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("release");
        let metadata = fs::read(release_root.join("latest.json")).unwrap();
        let metadata: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
        assert_eq!(metadata["version"], env!("CARGO_PKG_VERSION"));

        for location in [
            &metadata["platforms"]["windows-x86_64"],
            &metadata["portable"]["windows-x86_64"],
        ] {
            let url = Url::parse(location["url"].as_str().unwrap()).unwrap();
            let filename = url.path_segments().unwrap().next_back().unwrap();
            let asset_path = release_root.join(filename);
            assert!(asset_path.is_file(), "missing {}", asset_path.display());
            assert_eq!(
                hash_file_sha256(&asset_path).unwrap(),
                location["sha256"].as_str().unwrap()
            );
            verify_minisign(&asset_path, location["signature"].as_str().unwrap()).unwrap();
        }
    }
}
