use std::{
    fs::{self, File, OpenOptions as SyncOpenOptions},
    io::{BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
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
    io::AsyncWriteExt,
};

use crate::{
    error::{AppError, AppResult},
    model::{InstallRequest, PlannedAction},
    process::{output_text, run_capture},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
            "安装完成并已通过版本验证",
            1,
            Some(1),
        );
        Ok(OperationResult {
            operation_id: operation_id.to_string(),
            status: "committed".to_string(),
            message: format!("{} {} 安装完成", request.tool_id, request.version),
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

        let existing = tokio::fs::metadata(&partial)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let mut request_builder = self.client.get(&request.download_url);
        if existing > 0 {
            request_builder = request_builder.header(header::RANGE, format!("bytes={existing}-"));
        }
        let response = request_builder.send().await?.error_for_status()?;
        validate_final_download_host(response.url().host_str().unwrap_or_default())?;
        let resumed = existing > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
        let offset = if resumed { existing } else { 0 };
        let mut file = if resumed {
            OpenOptions::new().append(true).open(&partial).await?
        } else {
            AsyncFile::create(&partial).await?
        };
        let response_length = response.content_length();
        let total = response_length.map(|length| length + offset);
        let mut received = offset;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            received += chunk.len() as u64;
            emit_progress(
                app,
                operation_id,
                "download",
                if resumed {
                    "正在断点续传"
                } else {
                    "正在下载官方发行包"
                },
                received,
                total,
            );
        }
        file.flush().await?;
        drop(file);

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
        let output = run_capture(&program, &args, Duration::from_secs(20))?;
        if output.status.success() || !output_text(&output).is_empty() {
            return Ok(());
        }
    }
    Err(AppError::Message(format!(
        "安装后未找到或无法执行 {} 的预期命令",
        request.tool_id
    )))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{plugins::VersionSourceKind, sources};
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
