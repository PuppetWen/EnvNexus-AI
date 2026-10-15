use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions as SyncOpenOptions},
    io::{BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{Mutex, atomic::{AtomicBool, AtomicU64, Ordering}},
    time::{Duration, Instant},
};

use chrono::Utc;
use futures_util::StreamExt;
use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use tauri::{AppHandle, Emitter};
use tokio::{
    fs::{File as AsyncFile, OpenOptions},
    io::{AsyncWriteExt, BufWriter as AsyncBufWriter},
};

use crate::{
    environment::{EnvironmentMap, current_process_environment},
    error::{AppError, AppResult},
    model::{InstallRequest, PlannedAction},
    process::{output_text, run_capture, run_capture_with_env},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const DOWNLOAD_MAX_ATTEMPTS: u32 = 8;
const DOWNLOAD_BUFFER_BYTES: usize = 256 * 1024;
const PARALLEL_DOWNLOAD_MIN_BYTES: u64 = 8 * 1024 * 1024;
const PARALLEL_DOWNLOAD_STREAMS: u64 = 4;
const DOWNLOAD_PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationProgress {
    pub operation_id: String,
    pub phase: String,
    pub message: String,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationResult {
    pub operation_id: String,
    pub status: String,
    pub message: String,
    pub installation_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallManifest {
    pub schema_version: u32,
    pub operation_id: String,
    pub tool_id: String,
    pub version: String,
    pub installed_at: chrono::DateTime<Utc>,
    pub managed_root: PathBuf,
    pub installation_path: PathBuf,
    pub source_url: String,
    pub checksum_algorithm: Option<String>,
    pub checksum: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransactionJournal {
    schema_version: u32,
    operation_id: String,
    tool_id: String,
    operation: String,
    state: String,
    destination: PathBuf,
    updated_at: chrono::DateTime<Utc>,
    error: Option<String>,
}

#[derive(Clone)]
pub struct Installer {
    client: reqwest::Client,
    data_root: PathBuf,
}

impl Installer {
    pub fn new(client: reqwest::Client, data_root: PathBuf) -> Self {
        Self { client, data_root }
    }

    pub async fn execute(
        &self,
        operation_id: &str,
        action: &PlannedAction,
        app: &AppHandle,
    ) -> AppResult<OperationResult> {
        self.execute_inner(operation_id, action, Some(app)).await
    }

    pub async fn execute_headless(
        &self,
        operation_id: &str,
        action: &PlannedAction,
    ) -> AppResult<OperationResult> {
        self.execute_inner(operation_id, action, None).await
    }

    async fn execute_inner(
        &self,
        operation_id: &str,
        action: &PlannedAction,
        app: Option<&AppHandle>,
    ) -> AppResult<OperationResult> {
        let result = match action {
            PlannedAction::Install(request) => self.install(operation_id, request, app).await,
            PlannedAction::Repair(request) => self.repair(operation_id, request, app).await,
            PlannedAction::Uninstall {
                tool_id,
                installation_path,
                ..
            } => self.uninstall(operation_id, tool_id, installation_path, app),
            _ => Err(AppError::Message("安装执行器不支持该计划动作".to_string())),
        };
        if result.is_err() {
            let path = match action {
                PlannedAction::Install(request) | PlannedAction::Repair(request) => {
                    &request.destination
                }
                PlannedAction::Uninstall {
                    installation_path, ..
                } => installation_path,
                _ => Path::new("."),
            };
            self.log(operation_id, "ERROR", "operation_failed", path);
        }
        result
    }

    pub fn load_manifest(&self, installation_path: &Path) -> AppResult<InstallManifest> {
        read_manifest(installation_path)
    }

    async fn install(
        &self,
        operation_id: &str,
        request: &InstallRequest,
        app: Option<&AppHandle>,
    ) -> AppResult<OperationResult> {
        let mut journal = TransactionJournal {
            schema_version: 1,
            operation_id: operation_id.to_string(),
            tool_id: request.tool_id.clone(),
            operation: "install".to_string(),
            state: "confirmed".to_string(),
            destination: request.destination.clone(),
            updated_at: Utc::now(),
            error: None,
        };
        self.write_journal(&journal)?;
        self.log(
            operation_id,
            "INFO",
            "install_confirmed",
            &request.destination,
        );

        let result = self
            .install_inner(operation_id, request, app, &mut journal)
            .await;
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                journal.state = "failed".to_string();
                journal.updated_at = Utc::now();
                journal.error = Some(error.to_string());
                let _ = self.write_journal(&journal);
                self.log(
                    operation_id,
                    "ERROR",
                    "install_failed",
                    &request.destination,
                );
                Err(error)
            }
        }
    }

    async fn install_inner(
        &self,
        operation_id: &str,
        request: &InstallRequest,
        app: Option<&AppHandle>,
        journal: &mut TransactionJournal,
    ) -> AppResult<OperationResult> {
        validate_destination_boundary(&request.root, &request.destination)?;
        if request.destination.exists() {
            return Err(AppError::Message(format!(
                "目标目录在确认后已被创建：{}",
                request.destination.display()
            )));
        }
        let archive = self.download(operation_id, request, app).await?;
        journal.state = "downloaded".to_string();
        journal.updated_at = Utc::now();
        self.write_journal(journal)?;

        emit_progress(
            app,
            operation_id,
            "extract",
            "正在安全解压到暂存目录",
            0,
            None,
        );
        let destination = request.destination.clone();
        let root = request.root.clone();
        let tool_id = request.tool_id.clone();
        let version = request.version.clone();
        let archive_clone = archive.clone();
        let operation_id_owned = operation_id.to_string();
        let staging = tokio::task::spawn_blocking(move || {
            prepare_staging(
                &operation_id_owned,
                &tool_id,
                &version,
                &root,
                &destination,
                &archive_clone,
            )
        })
        .await
        .map_err(|error| AppError::Message(format!("解压任务异常结束：{error}")))??;

        journal.state = "staged".to_string();
        journal.updated_at = Utc::now();
        self.write_journal(journal)?;
        let commit_result = commit_staging(&staging, &request.destination);
        if let Err(error) = commit_result {
            safe_remove_directory(&staging, &request.root);
            return Err(error);
        }

        let manifest = InstallManifest {
            schema_version: 1,
            operation_id: operation_id.to_string(),
            tool_id: request.tool_id.clone(),
            version: request.version.clone(),
            installed_at: Utc::now(),
            managed_root: request.root.clone(),
            installation_path: request.destination.clone(),
            source_url: request.download_url.clone(),
            checksum_algorithm: request.checksum_algorithm.clone(),
            checksum: request.checksum.clone(),
        };
        let manifest_path = request.destination.join(".envpilot-install.json");
        if let Err(error) = write_json_atomic(&manifest_path, &manifest) {
            safe_remove_directory(&request.destination, &request.root);
            return Err(error);
        }
        let central_manifest = self
            .data_root
            .join("config")
            .join("installations")
            .join(format!("{operation_id}.json"));
        if let Err(error) = write_json_atomic(&central_manifest, &manifest) {
            safe_remove_directory(&request.destination, &request.root);
            return Err(error);
        }
        if let Err(error) = verify_installation(request) {
            let _ = fs::remove_file(&central_manifest);
            safe_remove_directory(&request.destination, &request.root);
            return Err(error);
        }

        journal.state = "committed".to_string();
        journal.updated_at = Utc::now();
        self.write_journal(journal)?;
        self.log(
            operation_id,
            "INFO",
            "install_committed",
            &request.destination,
        );
        emit_progress(
            app,
            operation_id,
            "complete",
            "安装完成并已通过版本验证；用户环境尚未配置",
            1,
            Some(1),
        );
        Ok(OperationResult {
            operation_id: operation_id.to_string(),
            status: "committed".to_string(),
            message: format!(
                "{} {} 安装完成；用户环境尚未配置",
                request.tool_id, request.version
            ),
            installation_path: Some(request.destination.clone()),
        })
    }

    async fn download(
        &self,
        operation_id: &str,
        request: &InstallRequest,
        app: Option<&AppHandle>,
    ) -> AppResult<PathBuf> {
        let directory = self.data_root.join("downloads").join(&request.tool_id);
        tokio::fs::create_dir_all(&directory).await?;
        let filename = download_filename(&request.download_url)?;
        let completed = directory.join(&filename);
        let partial = directory.join(format!("{filename}.part"));

        if completed.is_file() {
            if verify_checksum(
                &completed,
                request.checksum_algorithm.as_deref(),
                request.checksum.as_deref(),
            )? {
                emit_progress(
                    app,
                    operation_id,
                    "verify",
                    "复用已校验的下载缓存",
                    completed.metadata()?.len(),
                    Some(completed.metadata()?.len()),
                );
                return Ok(completed);
            }
            tokio::fs::remove_file(&completed).await?;
        }

        let mut received = 0;
        let mut total = None;
        let mut allow_parallel = true;
        for attempt in 1..=DOWNLOAD_MAX_ATTEMPTS {
            let existing = tokio::fs::metadata(&partial)
                .await
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            received = existing;
            let mut request_builder = self
                .client
                .get(&request.download_url)
                .header(header::ACCEPT_ENCODING, "identity");
            if existing > 0 {
                request_builder =
                    request_builder.header(header::RANGE, format!("bytes={existing}-"));
            }
            let response = match request_builder
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
            {
                Ok(response) => response,
                Err(error)
                    if attempt < DOWNLOAD_MAX_ATTEMPTS && retryable_download_error(&error) =>
                {
                    emit_download_retry(app, operation_id, existing, total, attempt + 1);
                    tokio::time::sleep(download_retry_delay(attempt)).await;
                    continue;
                }
                Err(error) => return Err(download_failure(error, attempt, received)),
            };
            validate_final_download_host(response.url().host_str().unwrap_or_default())?;

            if existing == 0 && allow_parallel && attempt < DOWNLOAD_MAX_ATTEMPTS {
                if let Some(metadata) = parallel_download_metadata(&response, request) {
                    allow_parallel = false;
                    drop(response);
                    match self
                        .download_parallel(operation_id, request, &partial, &metadata, app)
                        .await
                    {
                        Ok(true) => {
                            received = metadata.total;
                            total = Some(metadata.total);
                            break;
                        }
                        // Some servers advertise ranges but answer a range request with 200.
                        // Discard every segment and restart through the ordinary streaming path.
                        Ok(false) => continue,
                        Err(AppError::Http(error)) if retryable_download_error(&error) => {
                            emit_download_retry(app, operation_id, 0, Some(metadata.total), attempt + 1);
                            tokio::time::sleep(download_retry_delay(attempt)).await;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }

            let resume_range = if response.status() == StatusCode::PARTIAL_CONTENT {
                let range = parse_content_range(response.headers());
                match range {
                    Some(range)
                        if existing > 0
                            && range.start == existing
                            && range.end + 1 == range.total
                            && response.content_length().is_none_or(|length| length == range.length()) => Some(range),
                    _ => return Err(AppError::InvalidSource(
                        "下载服务器返回了不匹配的续传范围，已停止以避免文件损坏".to_string(),
                    )),
                }
            } else {
                None
            };
            let resumed = resume_range.is_some();
            let offset = if resumed { existing } else { 0 };
            let file = if resumed {
                OpenOptions::new().append(true).open(&partial).await?
            } else {
                AsyncFile::create(&partial).await?
            };
            let mut file = AsyncBufWriter::with_capacity(DOWNLOAD_BUFFER_BYTES, file);
            let response_length = response.content_length();
            total = resume_range.map(|range| range.total).or(response_length);
            received = offset;
            let progress = DownloadProgress::new(app, operation_id, total, offset);
            let mut stream = response.bytes_stream();
            let mut stream_error = None;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(chunk) => {
                        if total.is_some_and(|expected| chunk.len() as u64 > expected.saturating_sub(received)) {
                            return Err(AppError::InvalidSource(
                                "下载服务器发送了超出声明长度的数据，已停止以避免文件损坏".to_string(),
                            ));
                        }
                        file.write_all(&chunk).await?;
                        received += chunk.len() as u64;
                        progress.advance(
                            chunk.len() as u64,
                            if resumed {
                                "正在断点续传"
                            } else {
                                "正在下载官方发行包"
                            },
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
            progress.finish(if resumed { "正在断点续传" } else { "正在下载官方发行包" });

            let incomplete = total.is_some_and(|expected| received < expected);
            if let Some(error) = stream_error {
                if attempt < DOWNLOAD_MAX_ATTEMPTS && retryable_download_error(&error) {
                    emit_download_retry(app, operation_id, received, total, attempt + 1);
                    tokio::time::sleep(download_retry_delay(attempt)).await;
                    continue;
                }
                return Err(download_failure(error, attempt, received));
            }
            if incomplete {
                if attempt < DOWNLOAD_MAX_ATTEMPTS {
                    emit_download_retry(app, operation_id, received, total, attempt + 1);
                    tokio::time::sleep(download_retry_delay(attempt)).await;
                    continue;
                }
                return Err(AppError::Message(format!(
                    "下载在 {received} 字节处提前结束，自动续传次数已用尽"
                )));
            }
            break;
        }

        emit_progress(
            app,
            operation_id,
            "verify",
            "正在验证下载校验值",
            received,
            total,
        );
        let partial_clone = partial.clone();
        let algorithm = request.checksum_algorithm.clone();
        let expected = request.checksum.clone();
        let verified = tokio::task::spawn_blocking(move || {
            verify_checksum(&partial_clone, algorithm.as_deref(), expected.as_deref())
        })
        .await
        .map_err(|error| AppError::Message(format!("校验任务异常结束：{error}")))??;
        if !verified {
            let actual = hash_file(
                &partial,
                request.checksum_algorithm.as_deref().unwrap_or("sha256"),
            )
            .unwrap_or_else(|_| "无法计算".to_string());
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(AppError::ChecksumMismatch {
                expected: request
                    .checksum
                    .clone()
                    .unwrap_or_else(|| "未提供".to_string()),
                actual,
            });
        }
        tokio::fs::rename(&partial, &completed).await?;
        Ok(completed)
    }

    async fn download_parallel(
        &self,
        operation_id: &str,
        request: &InstallRequest,
        partial: &Path,
        metadata: &ParallelDownloadMetadata,
        app: Option<&AppHandle>,
    ) -> AppResult<bool> {
        let parts = DownloadParts::create(partial)?;
        let progress = DownloadProgress::new(app, operation_id, Some(metadata.total), 0);
        let cancelled = AtomicBool::new(false);
        let segment_size = metadata.total.div_ceil(PARALLEL_DOWNLOAD_STREAMS);
        let downloads = parts.paths.iter().enumerate().map(|(index, path)| {
            let start = index as u64 * segment_size;
            let end = start.saturating_add(segment_size).min(metadata.total) - 1;
            let progress = &progress;
            let cancelled = &cancelled;
            async move {
                let result = async {
                let response = self.client.get(&request.download_url)
                    .header(header::ACCEPT_ENCODING, "identity")
                    .header(header::RANGE, format!("bytes={start}-{end}"))
                    .header(header::IF_RANGE, metadata.validator.value())
                    .send().await?;
                validate_final_download_host(response.url().host_str().unwrap_or_default())?;
                if response.status() == StatusCode::OK {
                    return Ok(false);
                }
                let response = response.error_for_status()?;
                let expected_range = DownloadRange { start, end, total: metadata.total };
                if response.status() != StatusCode::PARTIAL_CONTENT
                    || parse_content_range(response.headers()) != Some(expected_range)
                    || response.content_length().is_some_and(|length| length != expected_range.length())
                    || !identity_content_encoding(response.headers())
                {
                    return Err(AppError::InvalidSource(
                        "下载服务器返回了不匹配的分片范围，已停止以避免文件损坏".to_string(),
                    ));
                }
                if !metadata.validator.matches(response.headers()) {
                    return Err(AppError::InvalidSource(
                        "下载期间官方文件的实体标识发生变化，已丢弃分片，请重新查询版本后下载".to_string(),
                    ));
                }
                let mut file = AsyncBufWriter::with_capacity(DOWNLOAD_BUFFER_BYTES, AsyncFile::create(path).await?);
                let mut stream = response.bytes_stream();
                let mut received = 0_u64;
                while let Some(chunk) = stream.next().await {
                    if cancelled.load(Ordering::Relaxed) {
                        return Ok(false);
                    }
                    let chunk = chunk?;
                    if chunk.len() as u64 > expected_range.length().saturating_sub(received) {
                        return Err(AppError::InvalidSource("下载分片超出声明长度，已停止以避免文件损坏".to_string()));
                    }
                    file.write_all(&chunk).await?;
                    received += chunk.len() as u64;
                    progress.advance(chunk.len() as u64, "正在并行下载官方发行包（4 路）");
                }
                file.flush().await?;
                if received != expected_range.length() {
                    return Err(AppError::InvalidSource("下载分片提前结束，已停止以避免文件损坏".to_string()));
                }
                Ok(true)
                }.await;
                if !matches!(result, Ok(true)) {
                    cancelled.store(true, Ordering::Relaxed);
                }
                result
            }
        });
        // Await every scoped worker after cancellation. In particular, no Tokio file
        // operation may still be writing when the private directory is cleaned up.
        let completed = futures_util::future::join_all(downloads).await
            .into_iter().collect::<AppResult<Vec<bool>>>()?;
        if completed.iter().any(|completed| !completed) {
            return Ok(false);
        }
        let mut output = AsyncBufWriter::with_capacity(DOWNLOAD_BUFFER_BYTES, AsyncFile::create(partial).await?);
        for path in &parts.paths {
            let mut input = AsyncFile::open(path).await?;
            tokio::io::copy(&mut input, &mut output).await?;
        }
        output.flush().await?;
        progress.finish("官方发行包下载完成");
        Ok(true)
    }

    async fn repair(
        &self,
        operation_id: &str,
        request: &InstallRequest,
        app: Option<&AppHandle>,
    ) -> AppResult<OperationResult> {
        validate_destination_boundary(&request.root, &request.destination)?;
        let old_manifest = read_manifest(&request.destination)?;
        if old_manifest.tool_id != request.tool_id
            || old_manifest.version != request.version
            || fs::canonicalize(&old_manifest.managed_root)? != fs::canonicalize(&request.root)?
        {
            return Err(AppError::UnsafePath(request.destination.clone()));
        }
        let archive = self.download(operation_id, request, app).await?;
        emit_progress(
            app,
            operation_id,
            "repair",
            "正在创建并验证修复暂存版本",
            0,
            None,
        );
        let staging = {
            let operation_id = operation_id.to_string();
            let tool_id = request.tool_id.clone();
            let version = request.version.clone();
            let root = request.root.clone();
            let destination = request.destination.clone();
            let archive = archive.clone();
            tokio::task::spawn_blocking(move || {
                prepare_staging(
                    &operation_id,
                    &tool_id,
                    &version,
                    &root,
                    &destination,
                    &archive,
                )
            })
            .await
            .map_err(|error| AppError::Message(format!("修复解压任务异常结束：{error}")))??
        };
        let parent = request
            .destination
            .parent()
            .ok_or_else(|| AppError::UnsafePath(request.destination.clone()))?;
        let backup = parent.join(format!(".envpilot-repair-backup-{operation_id}"));
        if backup.exists() {
            safe_remove_directory(&staging, &request.root);
            return Err(AppError::UnsafePath(backup));
        }
        fs::rename(&request.destination, &backup)?;
        if let Err(error) = commit_staging(&staging, &request.destination) {
            let _ = fs::rename(&backup, &request.destination);
            safe_remove_directory(&staging, &request.root);
            return Err(error);
        }
        if let Err(error) = verify_installation(request) {
            safe_remove_directory(&request.destination, &request.root);
            let _ = fs::rename(&backup, &request.destination);
            return Err(AppError::Message(format!(
                "修复版本验证失败，已恢复旧目录：{error}"
            )));
        }
        let manifest = InstallManifest {
            schema_version: 1,
            operation_id: operation_id.to_string(),
            tool_id: request.tool_id.clone(),
            version: request.version.clone(),
            installed_at: Utc::now(),
            managed_root: request.root.clone(),
            installation_path: request.destination.clone(),
            source_url: request.download_url.clone(),
            checksum_algorithm: request.checksum_algorithm.clone(),
            checksum: request.checksum.clone(),
        };
        if let Err(error) = write_json_atomic(
            &request.destination.join(".envpilot-install.json"),
            &manifest,
        ) {
            safe_remove_directory(&request.destination, &request.root);
            let _ = fs::rename(&backup, &request.destination);
            return Err(error);
        }
        let central_directory = self.data_root.join("config").join("installations");
        let new_central = central_directory.join(format!("{operation_id}.json"));
        if let Err(error) = write_json_atomic(&new_central, &manifest) {
            safe_remove_directory(&request.destination, &request.root);
            let restored = fs::rename(&backup, &request.destination);
            return match restored {
                Ok(()) => Err(AppError::Message(format!(
                    "写入修复清单失败，已恢复旧目录：{error}"
                ))),
                Err(restore_error) => Err(AppError::Message(format!(
                    "写入修复清单失败且旧目录恢复失败：{error}；恢复错误：{restore_error}；备份仍位于 {}",
                    backup.display()
                ))),
            };
        }
        if old_manifest.operation_id != operation_id {
            let _ = fs::remove_file(
                central_directory.join(format!("{}.json", old_manifest.operation_id)),
            );
        }
        let cleanup_warning = if let Err(error) = fs::remove_dir_all(&backup) {
            self.log(
                operation_id,
                "WARN",
                "repair_backup_cleanup_failed",
                &backup,
            );
            Some(format!(
                "修复已提交，但旧目录清理失败：{error}；旧目录仍位于 {}",
                backup.display()
            ))
        } else {
            None
        };
        self.log(
            operation_id,
            "INFO",
            "repair_committed",
            &request.destination,
        );
        emit_progress(
            app,
            operation_id,
            "complete",
            "修复完成并通过版本验证",
            1,
            Some(1),
        );
        Ok(OperationResult {
            operation_id: operation_id.to_string(),
            status: if cleanup_warning.is_some() {
                "committed_with_warning"
            } else {
                "committed"
            }
            .to_string(),
            message: cleanup_warning
                .unwrap_or_else(|| format!("{} {} 修复完成", request.tool_id, request.version)),
            installation_path: Some(request.destination.clone()),
        })
    }

    fn uninstall(
        &self,
        operation_id: &str,
        tool_id: &str,
        installation_path: &Path,
        app: Option<&AppHandle>,
    ) -> AppResult<OperationResult> {
        let manifest = read_manifest(installation_path)?;
        if manifest.tool_id != tool_id {
            return Err(AppError::UnsafePath(installation_path.to_path_buf()));
        }
        let actual = fs::canonicalize(installation_path)?;
        let expected = fs::canonicalize(&manifest.installation_path)?;
        let root = fs::canonicalize(&manifest.managed_root)?;
        if actual != expected || !actual.starts_with(&root) || actual == root {
            return Err(AppError::UnsafePath(actual));
        }
        let parent = actual
            .parent()
            .ok_or_else(|| AppError::UnsafePath(actual.clone()))?;
        let quarantine = parent.join(format!(".envpilot-uninstall-{operation_id}"));
        if quarantine.exists() {
            return Err(AppError::UnsafePath(quarantine));
        }
        emit_progress(
            app,
            operation_id,
            "uninstall",
            "正在隔离受管版本目录",
            0,
            None,
        );
        fs::rename(&actual, &quarantine)?;
        if let Err(error) = fs::remove_dir_all(&quarantine) {
            let _ = fs::rename(&quarantine, &actual);
            return Err(AppError::Message(format!(
                "删除失败，已尝试恢复原目录：{error}"
            )));
        }
        let central_manifest = self
            .data_root
            .join("config")
            .join("installations")
            .join(format!("{}.json", manifest.operation_id));
        let _ = fs::remove_file(central_manifest);
        self.log(operation_id, "INFO", "uninstall_committed", &actual);
        emit_progress(app, operation_id, "complete", "卸载完成", 1, Some(1));
        Ok(OperationResult {
            operation_id: operation_id.to_string(),
            status: "committed".to_string(),
            message: format!("{tool_id} 受管版本已卸载"),
            installation_path: None,
        })
    }

    fn write_journal(&self, journal: &TransactionJournal) -> AppResult<()> {
        let path = self
            .data_root
            .join("transactions")
            .join(format!("{}.json", journal.operation_id));
        write_json_atomic(&path, journal)
    }

    fn log(&self, operation_id: &str, level: &str, event: &str, path: &Path) {
        let log_path = self
            .data_root
            .join("logs")
            .join(format!("operations-{}.jsonl", Utc::now().format("%Y-%m")));
        let value = serde_json::json!({
            "timestamp": Utc::now(),
            "operationId": operation_id,
            "level": level,
            "event": event,
            "path": path,
        });
        if let Ok(mut file) = SyncOpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
        {
            let _ = writeln!(file, "{value}");
        }
    }
}

fn download_filename(value: &str) -> AppResult<String> {
    let url = url::Url::parse(value)
        .map_err(|error| AppError::InvalidSource(format!("下载 URL 无效：{error}")))?;
    let filename = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::InvalidSource("下载 URL 缺少文件名".to_string()))?;
    if filename.contains(['/', '\\']) || filename == "." || filename == ".." {
        return Err(AppError::InvalidSource("下载文件名不安全".to_string()));
    }
    Ok(filename.to_string())
}

fn validate_final_download_host(host: &str) -> AppResult<()> {
    let host = host.to_ascii_lowercase();
    #[cfg(test)]
    if matches!(host.as_str(), "127.0.0.1" | "localhost") {
        return Ok(());
    }
    let allowed = [
        "www.python.org",
        "python.org",
        "api.adoptium.net",
        "go.dev",
        "dl.google.com",
        "static.rust-lang.org",
        "nodejs.org",
        "github.com",
        "objects.githubusercontent.com",
        "release-assets.githubusercontent.com",
        "services.gradle.org",
        "downloads.apache.org",
        "builds.dotnet.microsoft.com",
        "download.visualstudio.microsoft.com",
        "windows.php.net",
    ];
    if allowed.contains(&host.as_str()) {
        Ok(())
    } else {
        Err(AppError::InvalidSource(format!(
            "下载重定向到了未授权主机：{host}"
        )))
    }
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

fn download_retry_delay(failed_attempt: u32) -> Duration {
    Duration::from_millis(750 * 2_u64.pow(failed_attempt.saturating_sub(1).min(3)))
}

fn download_failure(error: reqwest::Error, attempt: u32, received: u64) -> AppError {
    AppError::Message(format!(
        "下载失败（已尝试 {attempt} 次，已下载 {received} 字节）：{}",
        AppError::Http(error)
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DownloadRange {
    start: u64,
    end: u64,
    total: u64,
}

impl DownloadRange {
    fn length(self) -> u64 {
        self.end - self.start + 1
    }
}

fn parse_content_range(headers: &header::HeaderMap) -> Option<DownloadRange> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let range = DownloadRange { start: start.parse().ok()?, end: end.parse().ok()?, total: total.parse().ok()? };
    (range.start <= range.end && range.end < range.total).then_some(range)
}

enum DownloadValidator {
    Etag(header::HeaderValue),
    LastModified(header::HeaderValue),
}

impl DownloadValidator {
    fn value(&self) -> &header::HeaderValue {
        match self {
            Self::Etag(value) | Self::LastModified(value) => value,
        }
    }

    fn matches(&self, headers: &header::HeaderMap) -> bool {
        let name = match self {
            Self::Etag(_) => header::ETAG,
            Self::LastModified(_) => header::LAST_MODIFIED,
        };
        headers.get(name) == Some(self.value())
    }
}

struct ParallelDownloadMetadata {
    total: u64,
    validator: DownloadValidator,
}

fn identity_content_encoding(headers: &header::HeaderMap) -> bool {
    headers.get(header::CONTENT_ENCODING).is_none_or(|value| value == "identity")
}

fn parallel_download_metadata(response: &reqwest::Response, request: &InstallRequest) -> Option<ParallelDownloadMetadata> {
    let total = response.content_length()?;
    if response.status() != StatusCode::OK
        || total < PARALLEL_DOWNLOAD_MIN_BYTES
        || !identity_content_encoding(response.headers())
        || response.headers().get(header::ACCEPT_RANGES)?.to_str().ok()? != "bytes"
        || request.checksum.as_deref().is_none_or(|value| value.trim().is_empty())
        || !matches!(request.checksum_algorithm.as_deref(), Some("sha256" | "sha512" | "sha1"))
    {
        return None;
    }
    let validator = response.headers().get(header::ETAG)
        .filter(|value| value.to_str().is_ok_and(|value| value.starts_with('"') && value.ends_with('"') && value.len() > 2))
        .cloned().map(DownloadValidator::Etag)
        .or_else(|| response.headers().get(header::LAST_MODIFIED)
            .filter(|value| value.to_str().is_ok_and(|value| !value.is_empty()))
            .cloned().map(DownloadValidator::LastModified))?;
    Some(ParallelDownloadMetadata { total, validator })
}

struct DownloadParts {
    directory: PathBuf,
    paths: Vec<PathBuf>,
}

impl DownloadParts {
    fn create(partial: &Path) -> AppResult<Self> {
        let parent = partial.parent().ok_or_else(|| AppError::UnsafePath(partial.to_path_buf()))?;
        let directory = parent.join(format!(".envnexus-parts-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let paths = (0..PARALLEL_DOWNLOAD_STREAMS).map(|index| directory.join(format!("{index}.part"))).collect();
        Ok(Self { directory, paths })
    }
}

impl Drop for DownloadParts {
    fn drop(&mut self) {
        // Never recursively delete or touch any other operation's temporary files.
        for path in &self.paths {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir(&self.directory);
    }
}

struct DownloadProgress<'a> {
    app: Option<&'a AppHandle>,
    operation_id: &'a str,
    total: Option<u64>,
    received: AtomicU64,
    last_emit: Mutex<Instant>,
}

impl<'a> DownloadProgress<'a> {
    fn new(app: Option<&'a AppHandle>, operation_id: &'a str, total: Option<u64>, received: u64) -> Self {
        Self { app, operation_id, total, received: AtomicU64::new(received), last_emit: Mutex::new(Instant::now()) }
    }

    fn advance(&self, bytes: u64, message: &str) {
        let received = self.received.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if let Ok(mut last_emit) = self.last_emit.lock() {
            if last_emit.elapsed() >= DOWNLOAD_PROGRESS_INTERVAL {
                emit_progress(self.app, self.operation_id, "download", message, received, self.total);
                *last_emit = Instant::now();
            }
        }
    }

    fn finish(&self, message: &str) {
        emit_progress(self.app, self.operation_id, "download", message, self.received.load(Ordering::Relaxed), self.total);
    }
}

fn verify_checksum(
    path: &Path,
    algorithm: Option<&str>,
    expected: Option<&str>,
) -> AppResult<bool> {
    match (algorithm, expected) {
        (Some(algorithm), Some(expected)) => {
            let actual = hash_file(path, algorithm)?;
            Ok(actual.eq_ignore_ascii_case(expected.trim()))
        }
        _ => Ok(true),
    }
}

fn hash_file(path: &Path, algorithm: &str) -> AppResult<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut buffer = vec![0u8; 1024 * 1024];
    match algorithm.to_ascii_lowercase().as_str() {
        "sha256" => {
            let mut hasher = Sha256::new();
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "sha1" => {
            let mut hasher = Sha1::new();
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "sha512" => {
            let mut hasher = Sha512::new();
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        other => Err(AppError::InvalidSource(format!(
            "不支持的校验算法：{other}"
        ))),
    }
}

fn prepare_staging(
    operation_id: &str,
    tool_id: &str,
    version: &str,
    root: &Path,
    destination: &Path,
    archive: &Path,
) -> AppResult<PathBuf> {
    validate_destination_boundary(root, destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::UnsafePath(destination.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".envpilot-staging-{operation_id}"));
    if staging.exists() {
        safe_remove_directory(&staging, root);
    }
    fs::create_dir(&staging)?;
    let filename = archive
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extraction = if tool_id == "rust" && filename.ends_with(".exe") {
        install_rust(tool_id, version, archive, &staging)
    } else if filename.ends_with(".zip") {
        extract_zip(archive, &staging)
    } else if filename.ends_with(".tar.gz") || filename.ends_with(".tgz") {
        extract_tar_gz(archive, &staging)
    } else if tool_id == "git" && filename.ends_with(".7z.exe") {
        extract_git_sfx(archive, &staging)
    } else if filename.ends_with(".7z") {
        extract_7z(archive, &staging)
    } else {
        Err(AppError::InvalidSource(format!(
            "不支持的发行包格式：{filename}"
        )))
    };
    if let Err(error) = extraction {
        safe_remove_directory(&staging, root);
        return Err(error);
    }
    Ok(staging)
}

fn install_rust(_tool_id: &str, version: &str, installer: &Path, staging: &Path) -> AppResult<()> {
    let cargo_home = staging.join("cargo");
    let rustup_home = staging.join("rustup");
    let toolchain = format!("{version}-x86_64-pc-windows-msvc");
    let mut command = std::process::Command::new(installer);
    command
        .args([
            "-y",
            "--no-modify-path",
            "--profile",
            "minimal",
            "--default-toolchain",
            &toolchain,
        ])
        .envs(current_process_environment()?)
        .env("CARGO_HOME", &cargo_home)
        .env("RUSTUP_HOME", &rustup_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "rustup-init 执行失败：{}",
            output_text(&output)
        )))
    }
}

fn extract_zip(archive: &Path, destination: &Path) -> AppResult<()> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|error| AppError::Message(format!("打开 ZIP 失败：{error}")))?;
    if zip.len() > 100_000 {
        return Err(AppError::InvalidSource("ZIP 文件条目过多".to_string()));
    }
    let total_uncompressed = (0..zip.len())
        .filter_map(|index| zip.by_index_raw(index).ok().map(|entry| entry.size()))
        .sum::<u64>();
    if total_uncompressed > 20 * 1024 * 1024 * 1024 {
        return Err(AppError::InvalidSource(
            "ZIP 解压后体积超过 20 GiB 限制".to_string(),
        ));
    }

    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| AppError::Message(format!("读取 ZIP 条目失败：{error}")))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(AppError::InvalidSource(format!(
                "ZIP 包含路径穿越条目：{}",
                entry.name()
            )));
        };
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(AppError::InvalidSource(
                "ZIP 包含符号链接，已拒绝解压".to_string(),
            ));
        }
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output_file = File::create(&output)?;
        std::io::copy(&mut entry, &mut output_file)?;
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, destination: &Path) -> AppResult<()> {
    let file = File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(AppError::InvalidSource(
                "TAR 包含链接条目，已拒绝解压".to_string(),
            ));
        }
        if !entry.unpack_in(destination)? {
            return Err(AppError::InvalidSource("TAR 包含路径穿越条目".to_string()));
        }
    }
    Ok(())
}

fn extract_7z(archive: &Path, destination: &Path) -> AppResult<()> {
    let destination = destination.to_path_buf();
    let mut entry_count = 0usize;
    let mut total_uncompressed = 0u64;
    sevenz_rust::decompress_file_with_extract_fn(archive, &destination, |entry, reader, _| {
        entry_count += 1;
        total_uncompressed = total_uncompressed.saturating_add(entry.size());
        if entry_count > 100_000 || total_uncompressed > 20 * 1024 * 1024 * 1024 {
            return Err(sevenz_rust::Error::other(
                "7z 文件条目数量或解压体积超过安全限制",
            ));
        }
        let relative = Path::new(entry.name());
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir
                        | Component::CurDir
                        | Component::RootDir
                        | Component::Prefix(_)
                )
            })
        {
            return Err(sevenz_rust::Error::other(format!(
                "7z 包含路径穿越条目：{}",
                entry.name()
            )));
        }
        let output = destination.join(relative);
        sevenz_rust::default_entry_extract_fn(entry, reader, &output)
    })
    .map_err(|error| AppError::InvalidSource(format!("7z 安全解压失败：{error}")))
}

fn extract_git_sfx(archive: &Path, destination: &Path) -> AppResult<()> {
    let output_argument = format!("-o{}", destination.display());
    let output = run_capture(archive, &[&output_argument, "-y"], Duration::from_secs(180))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "PortableGit 解压失败：{}",
            output_text(&output)
        )))
    }
}

fn commit_staging(staging: &Path, destination: &Path) -> AppResult<()> {
    if destination.exists() {
        return Err(AppError::Message(format!(
            "提交前目标目录已存在：{}",
            destination.display()
        )));
    }
    let entries = fs::read_dir(staging)?
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    if entries.len() == 1 && entries[0].is_dir() {
        fs::rename(&entries[0], destination)?;
        fs::remove_dir(staging)?;
    } else {
        fs::rename(staging, destination)?;
    }
    Ok(())
}

fn verify_installation(request: &InstallRequest) -> AppResult<()> {
    let candidates = match request.tool_id.as_str() {
        "python" => vec![(request.destination.join("python.exe"), vec!["--version"])],
        "java" => vec![(
            request.destination.join("bin").join("java.exe"),
            vec!["-version"],
        )],
        "go" => vec![(
            request.destination.join("bin").join("go.exe"),
            vec!["version"],
        )],
        "rust" => vec![(
            request
                .destination
                .join("cargo")
                .join("bin")
                .join("rustc.exe"),
            vec!["--version"],
        )],
        "node" => vec![(request.destination.join("node.exe"), vec!["--version"])],
        "git" => vec![(
            request.destination.join("cmd").join("git.exe"),
            vec!["--version"],
        )],
        "android-sdk" => vec![(
            request.destination.join("bin").join("sdkmanager.bat"),
            vec!["--version"],
        )],
        "android-ndk" => vec![(request.destination.join("ndk-build.cmd"), vec!["--version"])],
        "gradle" => vec![(
            request.destination.join("bin").join("gradle.bat"),
            vec!["--version"],
        )],
        "cmake" => vec![(
            request.destination.join("bin").join("cmake.exe"),
            vec!["--version"],
        )],
        "adb" => vec![(request.destination.join("adb.exe"), vec!["version"])],
        "maven" => vec![(
            request.destination.join("bin").join("mvn.cmd"),
            vec!["--version"],
        )],
        "dotnet" => vec![(request.destination.join("dotnet.exe"), vec!["--version"])],
        "ruby" => vec![(
            request.destination.join("bin").join("ruby.exe"),
            vec!["--version"],
        )],
        "php" => vec![(request.destination.join("php.exe"), vec!["--version"])],
        _ => Vec::new(),
    };
    for (program, args) in candidates {
        if !program.is_file() {
            continue;
        }
        crate::tool_runtime::validate_runtime_dependencies(&request.tool_id, &program)?;
        let environment = installation_process_environment(request, current_process_environment()?);
        let environment = environment
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_os_str()))
            .collect::<Vec<_>>();
        let output = run_capture_with_env(&program, &args, Duration::from_secs(20), &environment)?;
        return verify_probe_output(&program, &output);
    }
    Err(AppError::Message(format!(
        "安装后未找到或无法执行 {} 的预期命令",
        request.tool_id
    )))
}

fn installation_process_environment(
    request: &InstallRequest,
    current: EnvironmentMap,
) -> Vec<(String, OsString)> {
    let mut environment = current
        .into_iter()
        .map(|(key, value)| (key, OsString::from(value)))
        .collect::<Vec<_>>();
    for (key, value) in installation_probe_environment(request) {
        environment.retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
        environment.push((key.to_string(), value));
    }
    environment
}

fn installation_probe_environment(request: &InstallRequest) -> Vec<(&'static str, OsString)> {
    if request.tool_id == "rust" {
        vec![
            (
                "CARGO_HOME",
                request.destination.join("cargo").into_os_string(),
            ),
            (
                "RUSTUP_HOME",
                request.destination.join("rustup").into_os_string(),
            ),
            (
                "RUSTUP_TOOLCHAIN",
                format!("{}-x86_64-pc-windows-msvc", request.version).into(),
            ),
        ]
    } else {
        Vec::new()
    }
}

fn verify_probe_output(program: &Path, output: &std::process::Output) -> AppResult<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "安装后命令验证失败（{}，退出状态 {}）：{}",
            program.display(),
            output.status,
            output_text(output)
        )))
    }
}

fn validate_destination_boundary(root: &Path, destination: &Path) -> AppResult<()> {
    let root = fs::canonicalize(root)?;
    if !destination.is_absolute()
        || destination
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(AppError::UnsafePath(destination.to_path_buf()));
    }
    if destination.exists() {
        let destination = fs::canonicalize(destination)?;
        if destination == root || !destination.starts_with(&root) {
            return Err(AppError::UnsafePath(destination));
        }
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::UnsafePath(destination.to_path_buf()))?;
    let mut existing = parent;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| AppError::UnsafePath(destination.to_path_buf()))?;
    }
    let existing = fs::canonicalize(existing)?;
    if !existing.starts_with(&root) {
        return Err(AppError::UnsafePath(destination.to_path_buf()));
    }
    Ok(())
}

fn safe_remove_directory(path: &Path, root: &Path) {
    let Ok(root) = fs::canonicalize(root) else {
        return;
    };
    let Ok(path) = fs::canonicalize(path) else {
        return;
    };
    if path != root && path.starts_with(&root) {
        let _ = fs::remove_dir_all(path);
    }
}

fn read_manifest(installation_path: &Path) -> AppResult<InstallManifest> {
    let path = installation_path.join(".envpilot-install.json");
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(AppError::from)
}

pub fn managed_manifests(data_root: &Path) -> Vec<InstallManifest> {
    let directory = data_root.join("config").join("installations");
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<InstallManifest>(&bytes).ok())
        .filter(|manifest| {
            manifest
                .installation_path
                .join(".envpilot-install.json")
                .is_file()
                && manifest
                    .installation_path
                    .starts_with(&manifest.managed_root)
        })
        .collect()
}

pub fn manifest_executables(manifest: &InstallManifest) -> Vec<PathBuf> {
    let root = &manifest.installation_path;
    match manifest.tool_id.as_str() {
        "python" => vec![root.join("python.exe")],
        "java" => vec![root.join("bin").join("java.exe")],
        "go" => vec![root.join("bin").join("go.exe")],
        "rust" => vec![root.join("cargo").join("bin").join("rustc.exe")],
        "node" => vec![root.join("node.exe")],
        "git" => vec![
            root.join("cmd").join("git.exe"),
            root.join("bin").join("git.exe"),
        ],
        "android-sdk" => vec![root.join("bin").join("sdkmanager.bat")],
        "android-ndk" => vec![root.join("ndk-build.cmd")],
        "gradle" => vec![root.join("bin").join("gradle.bat")],
        "cmake" => vec![root.join("bin").join("cmake.exe")],
        "adb" => vec![root.join("adb.exe")],
        "maven" => vec![root.join("bin").join("mvn.cmd")],
        "dotnet" => vec![root.join("dotnet.exe")],
        "ruby" => vec![root.join("bin").join("ruby.exe")],
        "php" => vec![root.join("php.exe")],
        _ => Vec::new(),
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    if !path.exists() {
        fs::rename(temporary, path)?;
        return Ok(());
    }
    let previous = path.with_extension("envpilot.previous");
    if previous.exists() {
        fs::remove_file(&previous)?;
    }
    fs::rename(path, &previous)?;
    match fs::rename(&temporary, path) {
        Ok(()) => {
            let _ = fs::remove_file(previous);
            Ok(())
        }
        Err(error) => {
            let _ = fs::rename(&previous, path);
            Err(error.into())
        }
    }
}

fn emit_progress(
    app: Option<&AppHandle>,
    operation_id: &str,
    phase: &str,
    message: &str,
    received: u64,
    total: Option<u64>,
) {
    let Some(app) = app else {
        return;
    };
    let percent = total
        .filter(|total| *total > 0)
        .map(|total| (received as f64 / total as f64 * 100.0).clamp(0.0, 100.0));
    let _ = app.emit(
        "operation-progress",
        OperationProgress {
            operation_id: operation_id.to_string(),
            phase: phase.to_string(),
            message: message.to_string(),
            received_bytes: received,
            total_bytes: total,
            percent,
        },
    );
}

fn emit_download_retry(
    app: Option<&AppHandle>,
    operation_id: &str,
    received: u64,
    total: Option<u64>,
    next_attempt: u32,
) {
    emit_progress(
        app,
        operation_id,
        "download",
        &format!("网络中断，正在自动续传（第 {next_attempt}/{DOWNLOAD_MAX_ATTEMPTS} 次）"),
        received,
        total,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{plugins::VersionSourceKind, sources};
    use std::{
        net::TcpListener,
        sync::{Arc, Mutex},
    };
    use tempfile::tempdir;

    #[test]
    fn hashes_sha512_sha256_and_sha1() {
        let temp = tempdir().unwrap();
        let file = temp.path().join("fixture.bin");
        fs::write(&file, b"envpilot").unwrap();
        assert_eq!(
            hash_file(&file, "sha256").unwrap(),
            "09248d1abb51a83c42fabc63be828d762713d118860f7624336016bf4c0e9765"
        );
        assert_eq!(
            hash_file(&file, "sha1").unwrap(),
            "ace09a0dbf08d83263c5e6d9d5204ee666f3b157"
        );
        assert_eq!(
            hash_file(&file, "sha512").unwrap(),
            "282c42b2a5512a26d68bc161c9590b8ee69c9fc3cd72272219322e20f3660d82bde4fc314ead911f31158d4e82b6533c654fed615f3eb766d7c3ce926d9e681e"
        );
    }

    #[test]
    fn rejects_unsafe_download_filename() {
        assert!(download_filename("https://example.com/").is_err());
    }

    #[test]
    fn destination_must_stay_below_root() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        assert!(validate_destination_boundary(&root, &root.join("node").join("22")).is_ok());
        assert!(
            validate_destination_boundary(&root, temp.path().join("outside").as_path()).is_err()
        );
        assert!(
            validate_destination_boundary(&root, &root.join("node").join("..").join("outside"))
                .is_err()
        );
    }

    #[test]
    fn managed_rust_probe_uses_its_own_homes_and_requested_toolchain() {
        let request = InstallRequest {
            tool_id: "rust".to_string(),
            version: "1.99.0".to_string(),
            root: PathBuf::from(r"E:\Environment"),
            destination: PathBuf::from(r"E:\Environment\rust\1.99.0"),
            download_url:
                "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe"
                    .to_string(),
            checksum_algorithm: None,
            checksum: None,
        };
        let environment = installation_probe_environment(&request);
        assert_eq!(
            environment[0],
            (
                "CARGO_HOME",
                request.destination.join("cargo").into_os_string()
            )
        );
        assert_eq!(
            environment[1],
            (
                "RUSTUP_HOME",
                request.destination.join("rustup").into_os_string()
            )
        );
        assert_eq!(
            environment[2],
            (
                "RUSTUP_TOOLCHAIN",
                OsString::from("1.99.0-x86_64-pc-windows-msvc")
            )
        );
    }

    #[test]
    fn probe_uses_fresh_java_home_and_overlays_managed_rust_homes() {
        let request = InstallRequest {
            tool_id: "rust".to_string(),
            version: "1.99.0".to_string(),
            root: PathBuf::from(r"E:\Environment"),
            destination: PathBuf::from(r"E:\Environment\rust\1.99.0"),
            download_url: String::new(),
            checksum_algorithm: None,
            checksum: None,
        };
        let current = EnvironmentMap::from([
            (
                "JAVA_HOME".to_string(),
                r"E:\Environment\java\27".to_string(),
            ),
            (
                "Path".to_string(),
                r"E:\Environment\java\27\bin".to_string(),
            ),
            ("rustup_home".to_string(), r"C:\old-rust".to_string()),
        ]);
        let environment = installation_process_environment(&request, current);
        assert!(environment.contains(&(
            "JAVA_HOME".to_string(),
            OsString::from(r"E:\Environment\java\27")
        )));
        assert!(environment.contains(&(
            "Path".to_string(),
            OsString::from(r"E:\Environment\java\27\bin")
        )));
        let rustup_homes = environment
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("RUSTUP_HOME"))
            .collect::<Vec<_>>();
        assert_eq!(rustup_homes.len(), 1);
        assert_eq!(
            rustup_homes[0].1,
            request.destination.join("rustup").into_os_string()
        );
    }

    #[cfg(windows)]
    #[test]
    fn failed_probe_stderr_is_an_error_not_installation_success() {
        use std::os::windows::process::ExitStatusExt;
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: Vec::new(),
            stderr: b"JAVA_HOME is not defined correctly".to_vec(),
        };
        let error = verify_probe_output(Path::new("mvn.cmd"), &output)
            .unwrap_err()
            .to_string();
        assert!(error.contains("JAVA_HOME is not defined correctly"));
        assert!(error.contains("mvn.cmd"));
    }

    #[cfg(windows)]
    #[test]
    fn successful_probe_may_report_version_on_stderr() {
        use std::os::windows::process::ExitStatusExt;
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: b"openjdk version 27".to_vec(),
        };
        assert!(verify_probe_output(Path::new("java.exe"), &output).is_ok());
    }

    #[derive(Clone, Copy)]
    enum RangeFixture {
        Supported,
        Unsupported,
        WrongRange,
        ChangedEntity,
    }

    fn check_parallel_download(fixture: RangeFixture) {
        use std::sync::atomic::AtomicUsize;

        let payload = Arc::new((0..PARALLEL_DOWNLOAD_MIN_BYTES as usize)
            .map(|index| (index % 251) as u8).collect::<Vec<_>>());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let full_requests = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let server_ranges = ranges.clone();
        let server_payload = payload.clone();
        let server_max_active = max_active.clone();
        let request_count = if matches!(fixture, RangeFixture::Unsupported) { 6 } else { 5 };
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut workers = Vec::new();
            while workers.len() < request_count {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing download requests");
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("test HTTP server failed: {error}"),
                };
                let payload = server_payload.clone();
                let ranges = server_ranges.clone();
                let full_requests = full_requests.clone();
                let active = active.clone();
                let max_active = server_max_active.clone();
                workers.push(std::thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let count = stream.read(&mut buffer).unwrap();
                        assert!(count > 0);
                        request.extend_from_slice(&buffer[..count]);
                    }
                    let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                    let range = request.lines().find_map(|line| line.strip_prefix("range: bytes="));
                    if let Some(range) = range {
                        assert!(request.contains("if-range: \"fixture\""));
                        ranges.lock().unwrap().push(range.to_string());
                        let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(concurrent, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        if matches!(fixture, RangeFixture::Unsupported) {
                            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
                        } else {
                            let (start, end) = range.split_once('-').unwrap();
                            let start = start.parse::<usize>().unwrap();
                            let end = end.parse::<usize>().unwrap();
                            let response_start = if matches!(fixture, RangeFixture::WrongRange) { start + 1 } else { start };
                            let etag = if matches!(fixture, RangeFixture::ChangedEntity) { "changed" } else { "fixture" };
                            let headers = format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nETag: \"{etag}\"\r\nConnection: close\r\n\r\n", end - start + 1, response_start, end, payload.len());
                            let _ = stream.write_all(headers.as_bytes());
                            if matches!(fixture, RangeFixture::Supported) {
                                let _ = stream.write_all(&payload[start..=end]);
                            }
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
                    } else {
                        let number = full_requests.fetch_add(1, Ordering::SeqCst);
                        let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n", payload.len());
                        stream.write_all(headers.as_bytes()).unwrap();
                        if number > 0 {
                            stream.write_all(&payload).unwrap();
                        }
                    }
                }));
            }
            for worker in workers { worker.join().unwrap(); }
        });
        let temp = tempdir().unwrap();
        let data_root = temp.path().join("data");
        let request = InstallRequest {
            tool_id: "python".to_string(), version: "3.14.6".to_string(),
            root: temp.path().join("tools"), destination: temp.path().join("tools/python/3.14.6"),
            download_url: format!("http://{address}/python.zip"),
            checksum_algorithm: Some("sha256".to_string()), checksum: Some(hex::encode(Sha256::digest(payload.as_slice()))),
        };
        let client = reqwest::Client::builder().no_proxy().connect_timeout(Duration::from_secs(2))
            .read_timeout(Duration::from_secs(2)).build().unwrap();
        let installer = Installer::new(client, data_root.clone());
        let result = tokio::runtime::Runtime::new().unwrap().block_on(installer.download("parallel-test", &request, None));
        server.join().unwrap();
        match fixture {
            RangeFixture::Supported | RangeFixture::Unsupported => assert_eq!(fs::read(result.unwrap()).unwrap(), *payload),
            RangeFixture::WrongRange => assert!(result.unwrap_err().to_string().contains("分片范围")),
            RangeFixture::ChangedEntity => assert!(result.unwrap_err().to_string().contains("实体标识")),
        }
        assert_eq!(ranges.lock().unwrap().len(), PARALLEL_DOWNLOAD_STREAMS as usize);
        if matches!(fixture, RangeFixture::Supported) {
            assert!(max_active.load(Ordering::SeqCst) >= 2, "range requests must overlap");
            let mut actual = ranges.lock().unwrap().clone();
            actual.sort();
            assert_eq!(actual, ["0-2097151", "2097152-4194303", "4194304-6291455", "6291456-8388607"]);
        }
        for entry in fs::read_dir(data_root.join("downloads/python")).unwrap() {
            assert!(!entry.unwrap().file_name().to_string_lossy().starts_with(".envnexus-parts-"));
        }
        if matches!(fixture, RangeFixture::WrongRange | RangeFixture::ChangedEntity) {
            assert!(!data_root.join("downloads/python/python.zip").exists());
            assert!(!data_root.join("downloads/python/python.zip.part").exists());
        }
    }

    #[test]
    fn large_download_uses_validated_concurrent_ranges_and_checksum() {
        check_parallel_download(RangeFixture::Supported);
    }

    #[test]
    fn server_ignoring_ranges_falls_back_to_a_fresh_single_stream() {
        check_parallel_download(RangeFixture::Unsupported);
    }

    #[test]
    fn parallel_download_rejects_wrong_range_and_cleans_private_parts() {
        check_parallel_download(RangeFixture::WrongRange);
    }

    #[test]
    fn parallel_download_rejects_changed_entity_and_cleans_private_parts() {
        check_parallel_download(RangeFixture::ChangedEntity);
    }

    #[test]
    fn interrupted_download_resumes_without_restarting_the_operation() {
        let payload = (0..128 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let first_chunk_length = 32 * 1024;
        let expected_checksum = hex::encode(Sha256::digest(&payload));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let observed_ranges = Arc::new(Mutex::new(Vec::new()));
        let server_ranges = observed_ranges.clone();
        let server_payload = payload.clone();
        let server = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let range = request
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("range: ")
                            .or_else(|| line.strip_prefix("Range: "))
                    })
                    .map(str::trim)
                    .map(str::to_string);
                server_ranges.lock().unwrap().push(range.clone());

                if attempt == 0 {
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        server_payload.len()
                    );
                    stream.write_all(headers.as_bytes()).unwrap();
                    stream
                        .write_all(&server_payload[..first_chunk_length])
                        .unwrap();
                } else {
                    assert_eq!(range.as_deref(), Some("bytes=32768-"));
                    let remaining = &server_payload[first_chunk_length..];
                    let headers = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        remaining.len(),
                        first_chunk_length,
                        server_payload.len() - 1,
                        server_payload.len()
                    );
                    stream.write_all(headers.as_bytes()).unwrap();
                    stream.write_all(remaining).unwrap();
                }
            }
        });

        let temp = tempdir().unwrap();
        let data_root = temp.path().join("data");
        let request = InstallRequest {
            tool_id: "python".to_string(),
            version: "3.14.6".to_string(),
            root: temp.path().join("tools"),
            destination: temp.path().join("tools").join("python").join("3.14.6"),
            download_url: format!("http://{address}/python.zip"),
            checksum_algorithm: Some("sha256".to_string()),
            checksum: Some(expected_checksum),
        };
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .read_timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let installer = Installer::new(client, data_root);
        let completed = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(installer.download("resume-test", &request, None))
            .unwrap();

        server.join().unwrap();
        assert_eq!(fs::read(completed).unwrap(), payload);
        assert_eq!(
            observed_ranges.lock().unwrap().as_slice(),
            &[None, Some("bytes=32768-".to_string())]
        );
    }

    #[test]
    #[ignore = "downloads and installs a live official Python package"]
    fn live_python_install_transaction_commits_verified_version() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let artifact_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("artifacts")
                .join("integration");
            fs::create_dir_all(&artifact_root).unwrap();
            let temp = tempfile::Builder::new()
                .prefix("python-install-")
                .tempdir_in(&artifact_root)
                .unwrap();
            let managed_root = temp.path().join("tools");
            let data_root = temp.path().join("data");
            fs::create_dir_all(&managed_root).unwrap();
            let client = reqwest::Client::builder()
                .user_agent("EnvNexus-AI-live-install-test/0.1.0")
                .https_only(true)
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap();
            let catalog = sources::fetch(&client, "python", VersionSourceKind::Python)
                .await
                .unwrap();
            let remote = catalog
                .versions
                .into_iter()
                .find(|version| {
                    version.download_url.is_some()
                        && version.checksum_algorithm.as_deref() == Some("sha256")
                        && version.checksum.is_some()
                })
                .expect("official Python catalog returned no verified Windows package");
            let destination = managed_root.join("python").join(&remote.version);
            let request = InstallRequest {
                tool_id: "python".to_string(),
                version: remote.version,
                root: managed_root,
                destination: destination.clone(),
                download_url: remote.download_url.unwrap(),
                checksum_algorithm: remote.checksum_algorithm,
                checksum: remote.checksum,
            };
            println!(
                "live install: Python {} from {} ({})",
                request.version,
                request.download_url,
                request.checksum.as_deref().unwrap_or("no checksum")
            );
            let installer = Installer::new(client, data_root);
            let result = installer
                .install("live-python-e2e", &request, None)
                .await
                .unwrap();
            assert_eq!(result.status, "committed");
            assert!(destination.join("python.exe").is_file());
            let manifest = read_manifest(&destination).unwrap();
            assert_eq!(manifest.version, request.version);
            verify_installation(&request).unwrap();
        });
    }
}
