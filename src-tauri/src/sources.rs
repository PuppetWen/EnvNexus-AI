use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use futures_util::{StreamExt, stream};
use regex::Regex;
use serde::Deserialize;

use crate::{
    error::{AppError, AppResult},
    model::{RemoteVersion, VersionCatalog},
    plugins::VersionSourceKind,
};

pub async fn fetch(
    client: &reqwest::Client,
    tool_id: &str,
    kind: VersionSourceKind,
) -> AppResult<VersionCatalog> {
    let mut catalog = match kind {
        VersionSourceKind::Python => fetch_python(client, tool_id).await,
        VersionSourceKind::Adoptium => fetch_adoptium(client, tool_id).await,
        VersionSourceKind::Go => fetch_go(client, tool_id).await,
        VersionSourceKind::Rust => fetch_rust(client, tool_id).await,
        VersionSourceKind::Node => fetch_node(client, tool_id).await,
        VersionSourceKind::GitForWindows => {
            fetch_github_release(
                client,
                tool_id,
                "Git for Windows",
                "https://api.github.com/repos/git-for-windows/git/releases",
                select_portable_git,
            )
            .await
        }
        VersionSourceKind::AndroidSdk => {
            fetch_android(client, tool_id, AndroidPackage::CommandLineTools).await
        }
        VersionSourceKind::AndroidNdk => fetch_android(client, tool_id, AndroidPackage::Ndk).await,
        VersionSourceKind::Gradle => fetch_gradle(client, tool_id).await,
        VersionSourceKind::CMake => {
            fetch_github_release(
                client,
                tool_id,
                "Kitware CMake",
                "https://api.github.com/repos/Kitware/CMake/releases",
                select_cmake_zip,
            )
            .await
        }
        VersionSourceKind::Adb => {
            fetch_android(client, tool_id, AndroidPackage::PlatformTools).await
        }
        VersionSourceKind::Maven => fetch_maven(client, tool_id).await,
        VersionSourceKind::DotNet => fetch_dotnet(client, tool_id).await,
        VersionSourceKind::Ruby => fetch_ruby(client, tool_id).await,
        VersionSourceKind::Php => fetch_php(client, tool_id).await,
    }?;
    if catalog.versions.is_empty() {
        return Err(AppError::InvalidSource(format!(
            "{} 没有返回可安装的 Windows x64 版本",
            catalog.source_name
        )));
    }
    crate::versioning::sort_remote_versions_descending(&mut catalog.versions);
    Ok(catalog)
}

fn catalog(
    tool_id: &str,
    source_name: &str,
    source_url: &str,
    versions: Vec<RemoteVersion>,
) -> VersionCatalog {
    VersionCatalog {
        tool_id: tool_id.to_string(),
        source_name: source_name.to_string(),
        source_url: source_url.to_string(),
        fetched_at: Utc::now(),
        cached: false,
        versions,
    }
}

async fn checked_text(client: &reqwest::Client, url: &str) -> AppResult<String> {
    for attempt in 0..3 {
        let (error, retryable) = match checked_response(client, url, "text/plain, text/html;q=0.9, */*;q=0.8").await {
            Ok(response) => match response.text().await {
                Ok(text) if !text.is_empty() => return Ok(text),
                Ok(_) => (AppError::InvalidSource(format!("{url}：响应体为空")), true),
                Err(error) => (AppError::Http(error), true),
            },
            Err(error) => {
                let retryable = retryable_source_error(&error);
                (error, retryable)
            }
        };
        if !retryable || attempt == 2 {
            return Err(error);
        }
        retry_delay(attempt).await;
    }
    unreachable!("the final source request returns its error")
}

async fn checked_json<T>(client: &reqwest::Client, url: &str) -> AppResult<T>
where
    T: serde::de::DeserializeOwned,
{
    for attempt in 0..3 {
        let (error, retryable) = match checked_response(client, url, "application/json").await {
            Ok(response) => {
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("未知 Content-Type")
                    .to_string();
                match response.bytes().await {
                    Ok(bytes) if bytes.is_empty() => (AppError::InvalidSource(format!("{url}：响应体为空")), true),
                    Ok(bytes) => match serde_json::from_slice::<T>(&bytes) {
                        Ok(value) => return Ok(value),
                        Err(error) => {
                            let retryable = error.is_eof();
                            (AppError::InvalidSource(format!("{url}：JSON 数据不完整或格式已变化（{content_type}）：{error}")), retryable)
                        }
                    },
                    Err(error) => (AppError::Http(error), true),
                }
            }
            Err(error) => {
                let retryable = retryable_source_error(&error);
                (error, retryable)
            }
        };
        if !retryable || attempt == 2 {
            return Err(error);
        }
        retry_delay(attempt).await;
    }
    unreachable!("the final source request returns its error")
}

async fn checked_response(
    client: &reqwest::Client,
    url: &str,
    accept: &str,
) -> AppResult<reqwest::Response> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, accept)
        .send()
        .await?;
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let headers = response.headers();
    let rate_limited = status == 429
        || (status == 403
            && (headers.get("x-ratelimit-remaining").is_some_and(|value| value == "0")
                || headers.contains_key(reqwest::header::RETRY_AFTER)));
    let retry_at = if rate_limited || headers.contains_key(reqwest::header::RETRY_AFTER) {
        source_retry_time(headers, Utc::now())
    } else {
        None
    };
    let mut safe_url = response.url().clone();
    let _ = safe_url.set_username("");
    let _ = safe_url.set_password(None);
    safe_url.set_query(None);
    safe_url.set_fragment(None);
    Err(AppError::SourceHttp { status, url: safe_url.to_string(), retry_at, rate_limited })
}

fn source_retry_time(headers: &reqwest::header::HeaderMap, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(value) = headers.get(reqwest::header::RETRY_AFTER).and_then(|value| value.to_str().ok()) {
        if let Some(retry_at) = value.trim().parse::<i64>().ok()
            .filter(|seconds| *seconds >= 0)
            .and_then(Duration::try_seconds)
            .and_then(|delay| now.checked_add_signed(delay))
        {
            return Some(retry_at);
        }
        if let Ok(date) = DateTime::parse_from_rfc2822(value) {
            return Some(date.with_timezone(&Utc));
        }
    }
    headers.get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
}

fn retryable_source_error(error: &AppError) -> bool {
    match error {
        AppError::Http(error) => error.is_timeout() || error.is_connect() || error.is_body(),
        AppError::SourceHttp { status, rate_limited: false, retry_at, .. } => {
            (*status == 408 || (500..=599).contains(status))
                && retry_at.is_none_or(|at| at <= Utc::now())
        }
        _ => false,
    }
}

async fn retry_delay(attempt: usize) {
    if attempt < 2 {
        tokio::time::sleep(std::time::Duration::from_millis(350 * (attempt as u64 + 1))).await;
    }
}

#[derive(Debug, Clone, Deserialize)]
struct PythonRelease {
    name: String,
    release_date: String,
    pre_release: bool,
    release_notes_url: Option<String>,
    resource_uri: String,
}

#[derive(Debug, Deserialize)]
struct PythonReleaseFile {
    name: String,
    release: String,
    url: String,
    sha256_sum: String,
}

async fn fetch_python(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://www.python.org/api/v2/downloads/release/?is_published=true";
    const FILES_SOURCE: &str = "https://www.python.org/api/v2/downloads/release_file/";
    let releases = checked_json::<Vec<PythonRelease>>(client, SOURCE).await?;
    let files = checked_json::<Vec<PythonReleaseFile>>(client, FILES_SOURCE).await?;
    let versions = build_python_versions(releases, files);
    Ok(catalog(tool_id, "Python.org", SOURCE, versions))
}

fn build_python_versions(
    releases: Vec<PythonRelease>,
    files: Vec<PythonReleaseFile>,
) -> Vec<RemoteVersion> {
    let mut files_by_release = files
        .into_iter()
        .filter(|file| {
            file.name
                .to_ascii_lowercase()
                .contains("windows embeddable package (64-bit)")
        })
        .filter_map(|file| {
            let id = resource_id(&file.release)?.to_string();
            Some((id, file))
        })
        .collect::<HashMap<_, _>>();
    let mut versions = releases
        .into_iter()
        .filter(|release| release.name.starts_with("Python 3.") && !release.pre_release)
        .filter_map(|release| {
            let file = files_by_release.remove(resource_id(&release.resource_uri)?)?;
            Some(RemoteVersion {
                version: release.name.trim_start_matches("Python ").to_string(),
                channel: "stable".to_string(),
                published_at: Some(release.release_date),
                architecture: "x86_64".to_string(),
                download_url: Some(file.url),
                checksum_algorithm: non_empty("sha256"),
                checksum: non_empty(&file.sha256_sum),
                notes_url: release.release_notes_url,
            })
        })
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| right.published_at.cmp(&left.published_at));
    versions
}

fn resource_id(uri: &str) -> Option<&str> {
    uri.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|id| !id.is_empty())
}

#[derive(Debug, Deserialize)]
struct AdoptiumInfo {
    available_releases: Vec<u32>,
    available_lts_releases: Vec<u32>,
}

#[derive(Debug, Deserialize)]
struct AdoptiumAsset {
    binary: AdoptiumBinary,
    release_link: Option<String>,
    version: AdoptiumVersion,
}

#[derive(Debug, Deserialize)]
struct AdoptiumBinary {
    package: AdoptiumPackage,
    updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdoptiumPackage {
    checksum: String,
    link: String,
}

#[derive(Debug, Deserialize)]
struct AdoptiumVersion {
    openjdk_version: String,
}

async fn fetch_adoptium(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://api.adoptium.net/v3/info/available_releases";
    let info = checked_json::<AdoptiumInfo>(client, SOURCE).await?;
    let lts_releases = info
        .available_lts_releases
        .into_iter()
        .collect::<HashSet<_>>();
    let mut majors = info.available_releases;
    majors.sort_unstable_by(|left, right| right.cmp(left));
    majors.dedup();

    let client = client.clone();
    let results = stream::iter(majors)
        .map(move |major| {
            let client = client.clone();
            let is_lts = lts_releases.contains(&major);
            async move {
                let url = format!(
                    "https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type=jdk&os=windows&vendor=eclipse"
                );
                let asset = checked_json::<Vec<AdoptiumAsset>>(&client, &url)
                    .await?
                    .into_iter()
                    .next();
                Ok::<_, AppError>(asset.map(|asset| RemoteVersion {
                    version: asset.version.openjdk_version,
                    channel: if is_lts {
                        "LTS".to_string()
                    } else {
                        "feature".to_string()
                    },
                    published_at: asset.binary.updated_at,
                    architecture: "x86_64".to_string(),
                    download_url: Some(asset.binary.package.link),
                    checksum_algorithm: non_empty("sha256"),
                    checksum: non_empty(&asset.binary.package.checksum),
                    notes_url: asset.release_link,
                }))
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let versions = results
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .collect();
    Ok(catalog(tool_id, "Eclipse Adoptium", SOURCE, versions))
}

#[derive(Debug, Deserialize)]
struct GoRelease {
    version: String,
    stable: bool,
    files: Vec<GoFile>,
}

#[derive(Debug, Deserialize)]
struct GoFile {
    filename: String,
    os: String,
    arch: String,
    kind: String,
    sha256: String,
}

async fn fetch_go(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://go.dev/dl/?mode=json&include=all";
    let releases = checked_json::<Vec<GoRelease>>(client, SOURCE).await?;
    let versions = releases
        .into_iter()
        .filter_map(|release| {
            let file = release.files.into_iter().find(|file| {
                file.os == "windows"
                    && file.arch == "amd64"
                    && file.kind == "archive"
                    && file.filename.ends_with(".zip")
            })?;
            Some(RemoteVersion {
                version: release.version.trim_start_matches("go").to_string(),
                channel: if release.stable { "stable" } else { "preview" }.to_string(),
                published_at: None,
                architecture: "x86_64".to_string(),
                download_url: Some(format!("https://go.dev/dl/{}", file.filename)),
                checksum_algorithm: non_empty("sha256"),
                checksum: non_empty(&file.sha256),
                notes_url: Some(format!(
                    "https://go.dev/doc/devel/release#{}",
                    release.version
                )),
            })
        })
        .collect();
    Ok(catalog(tool_id, "Go Downloads", SOURCE, versions))
}

async fn fetch_rust(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://static.rust-lang.org/dist/channel-rust-stable.toml";
    let manifest = checked_text(client, SOURCE).await?;
    let value = toml::from_str::<toml::Value>(&manifest)?;
    let date = value.get("date").and_then(toml::Value::as_str);
    let package = value
        .get("pkg")
        .and_then(|value| value.get("rust"))
        .ok_or_else(|| AppError::InvalidSource("Rust stable package 缺失".to_string()))?;
    let version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| AppError::InvalidSource("Rust stable version 缺失".to_string()))?;
    let _target = package
        .get("target")
        .and_then(|value| value.get("x86_64-pc-windows-msvc"))
        .ok_or_else(|| AppError::InvalidSource("Rust Windows MSVC target 缺失".to_string()))?;
    let rustup_url =
        "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe";
    let rustup_hash = checked_text(
        client,
        "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe.sha256",
    )
    .await?
    .split_whitespace()
    .next()
    .map(str::to_string);
    let versions = vec![RemoteVersion {
        version: version.to_string(),
        channel: "stable".to_string(),
        published_at: date.map(str::to_string),
        architecture: "x86_64".to_string(),
        download_url: Some(rustup_url.to_string()),
        checksum_algorithm: non_empty("sha256"),
        checksum: rustup_hash,
        notes_url: Some("https://doc.rust-lang.org/stable/releases.html".to_string()),
    }];
    Ok(catalog(tool_id, "Rust Project", SOURCE, versions))
}

#[derive(Debug, Clone, Deserialize)]
struct NodeRelease {
    version: String,
    date: String,
    files: Vec<String>,
    lts: serde_json::Value,
}

async fn fetch_node(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://nodejs.org/dist/index.json";
    let releases = checked_json::<Vec<NodeRelease>>(client, SOURCE).await?;
    Ok(catalog(tool_id, "Node.js Releases", SOURCE, build_node_versions(releases)))
}

fn build_node_versions(releases: Vec<NodeRelease>) -> Vec<RemoteVersion> {
    let mut versions = releases
        .into_iter()
        .filter(|release| release.files.iter().any(|file| file == "win-x64-zip"))
        .map(|release| {
                let filename = format!("node-{}-win-x64.zip", release.version);
                RemoteVersion {
                    version: release.version.trim_start_matches('v').to_string(),
                    channel: if release.lts.is_string() {
                        "LTS"
                    } else {
                        "current"
                    }
                    .to_string(),
                    published_at: Some(release.date),
                    architecture: "x86_64".to_string(),
                    download_url: Some(format!(
                        "https://nodejs.org/dist/{}/{}",
                        release.version, filename
                    )),
                    // Fetch exactly the selected release's checksum before installation.
                    checksum_algorithm: None,
                    checksum: None,
                    notes_url: Some(format!(
                        "https://nodejs.org/en/blog/release/{}",
                        release.version
                    )),
                }
        })
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| right.published_at.cmp(&left.published_at));
    versions
}

pub(crate) async fn ensure_install_metadata(
    client: &reqwest::Client,
    tool_id: &str,
    remote: &mut RemoteVersion,
) -> AppResult<()> {
    if tool_id != "node" {
        return Ok(());
    }
    let version = remote.version.trim_start_matches('v');
    if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')) {
        return Err(AppError::InvalidSource("Node.js 版本号格式无效".to_string()));
    }
    let expected_url = format!("https://nodejs.org/dist/v{version}/node-v{version}-win-x64.zip");
    if remote.download_url.as_deref() != Some(expected_url.as_str()) {
        return Err(AppError::InvalidSource("Node.js 下载地址与所选版本的官方发行包不匹配".to_string()));
    }
    if remote.checksum_algorithm.as_deref().is_some_and(|value| value.eq_ignore_ascii_case("sha256"))
        && remote.checksum.as_deref().is_some_and(valid_sha256)
    {
        return Ok(());
    }
    let checksum_url = format!("https://nodejs.org/dist/v{version}/SHASUMS256.txt");
    fill_node_checksum(client, remote, &checksum_url).await
}

async fn fill_node_checksum(client: &reqwest::Client, remote: &mut RemoteVersion, checksum_url: &str) -> AppResult<()> {
    let filename = format!("node-v{}-win-x64.zip", remote.version.trim_start_matches('v'));
    let checksums = checked_text(client, checksum_url).await?;
    let matching = checksums.lines().filter_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == filename && parts.next().is_none()).then_some(hash)
    }).collect::<Vec<_>>();
    if matching.len() != 1 || !valid_sha256(matching[0]) {
        return Err(AppError::InvalidSource(format!("Node.js 官方校验清单缺少唯一有效的 {filename} SHA-256，已停止安装")));
    }
    remote.checksum_algorithm = Some("sha256".to_string());
    remote.checksum = Some(matching[0].to_ascii_lowercase());
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Deserialize)]
struct GradleRelease {
    version: String,
    #[serde(rename = "buildTime")]
    build_time: String,
    #[serde(rename = "downloadUrl")]
    download_url: String,
    checksum: Option<String>,
    broken: bool,
    snapshot: bool,
    nightly: bool,
    #[serde(rename = "activeRc")]
    active_rc: bool,
    #[serde(rename = "final")]
    final_: Option<bool>,
}

async fn fetch_gradle(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://services.gradle.org/versions/all";
    let releases = checked_json::<Vec<GradleRelease>>(client, SOURCE).await?;
    let versions = releases
        .into_iter()
        .filter(|release| {
            !release.broken
                && !release.snapshot
                && !release.nightly
                && (release.final_.unwrap_or(false) || release.active_rc)
        })
        .map(|release| RemoteVersion {
            version: release.version,
            channel: if release.active_rc { "RC" } else { "stable" }.to_string(),
            published_at: Some(release.build_time),
            architecture: "any".to_string(),
            download_url: Some(release.download_url),
            checksum_algorithm: release.checksum.as_ref().map(|_| "sha256".to_string()),
            checksum: release.checksum,
            notes_url: Some("https://docs.gradle.org/current/release-notes.html".to_string()),
        })
        .collect();
    Ok(catalog(tool_id, "Gradle Services", SOURCE, versions))
}

async fn fetch_maven(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://downloads.apache.org/maven/maven-3/";
    let html = checked_text(client, SOURCE).await?;
    let version_pattern = Regex::new(r#"href="([0-9]+\.[0-9]+\.[0-9]+)/""#)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let candidates = version_pattern
        .captures_iter(&html)
        .filter_map(|capture| capture.get(1).map(|value| value.as_str().to_string()))
        .collect::<Vec<_>>();
    let client = client.clone();
    let versions = stream::iter(candidates)
        .map(move |version| {
            let client = client.clone();
            async move {
                let filename = format!("apache-maven-{version}-bin.zip");
                let download_url = format!(
                    "https://downloads.apache.org/maven/maven-3/{version}/binaries/{filename}"
                );
                let checksum = checked_text(&client, &format!("{download_url}.sha512"))
                    .await
                    .ok()
                    .and_then(|value| value.split_whitespace().next().map(str::to_string));
                RemoteVersion {
                    version,
                    channel: "stable".to_string(),
                    published_at: None,
                    architecture: "any".to_string(),
                    download_url: Some(download_url),
                    checksum_algorithm: checksum.as_ref().map(|_| "sha512".to_string()),
                    checksum,
                    notes_url: Some("https://maven.apache.org/docs/history.html".to_string()),
                }
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    Ok(catalog(tool_id, "Apache Maven Downloads", SOURCE, versions))
}

#[derive(Debug, Deserialize)]
struct DotNetReleaseIndex {
    #[serde(rename = "releases-index")]
    releases_index: Vec<DotNetChannel>,
}

#[derive(Debug, Clone, Deserialize)]
struct DotNetChannel {
    #[serde(rename = "channel-version")]
    channel_version: String,
    #[serde(rename = "release-type")]
    release_type: String,
    #[serde(rename = "support-phase")]
    support_phase: String,
    #[serde(rename = "releases.json")]
    releases_json: String,
}

#[derive(Debug, Deserialize)]
struct DotNetReleaseCatalog {
    releases: Vec<DotNetRelease>,
}

#[derive(Debug, Deserialize)]
struct DotNetRelease {
    #[serde(rename = "release-date")]
    release_date: String,
    #[serde(rename = "release-notes")]
    release_notes: Option<String>,
    sdk: Option<DotNetSdk>,
}

#[derive(Debug, Deserialize)]
struct DotNetSdk {
    version: String,
    files: Vec<DotNetFile>,
}

#[derive(Debug, Deserialize)]
struct DotNetFile {
    name: String,
    rid: String,
    url: String,
    hash: String,
}

async fn fetch_dotnet(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str =
        "https://builds.dotnet.microsoft.com/dotnet/release-metadata/releases-index.json";
    let mut channels = checked_json::<DotNetReleaseIndex>(client, SOURCE)
        .await?
        .releases_index;
    channels.retain(|channel| !channel.support_phase.eq_ignore_ascii_case("preview"));
    let client = client.clone();
    let results = stream::iter(channels)
        .map(move |channel| {
            let client = client.clone();
            async move {
                let catalog =
                    checked_json::<DotNetReleaseCatalog>(&client, &channel.releases_json).await?;
                let versions = catalog
                    .releases
                    .into_iter()
                    .filter_map(|release| {
                        let sdk = release.sdk?;
                        let file = sdk.files.into_iter().find(|file| {
                            file.rid == "win-x64"
                                && file.name.starts_with("dotnet-sdk-")
                                && file.name.ends_with("-win-x64.zip")
                        })?;
                        Some(RemoteVersion {
                            version: sdk.version,
                            channel: format!(
                                "{} {}",
                                channel.channel_version,
                                channel.release_type.to_ascii_uppercase()
                            ),
                            published_at: Some(release.release_date),
                            architecture: "x86_64".to_string(),
                            download_url: Some(file.url),
                            checksum_algorithm: non_empty("sha512"),
                            checksum: non_empty(&file.hash),
                            notes_url: release.release_notes,
                        })
                    })
                    .collect::<Vec<_>>();
                Ok::<_, AppError>(versions)
            }
        })
        .buffer_unordered(3)
        .collect::<Vec<_>>()
        .await;
    let mut versions = results
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| right.published_at.cmp(&left.published_at));
    Ok(catalog(
        tool_id,
        "Microsoft .NET Releases",
        SOURCE,
        versions,
    ))
}

async fn fetch_ruby(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://api.github.com/repos/oneclick/rubyinstaller2/releases";
    let releases = checked_github_releases(client, SOURCE).await?;
    let versions = releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let asset = select_ruby_archive(&release.assets)?;
            let version = asset
                .name
                .trim_start_matches("rubyinstaller-")
                .trim_end_matches("-x64.7z")
                .to_string();
            let (checksum_algorithm, checksum) = asset
                .digest
                .as_deref()
                .and_then(|value| value.split_once(':'))
                .map(|(algorithm, value)| {
                    (
                        Some(algorithm.to_ascii_lowercase()),
                        Some(value.to_string()),
                    )
                })
                .unwrap_or((None, None));
            Some(RemoteVersion {
                version,
                channel: if release.prerelease {
                    "preview"
                } else {
                    "stable"
                }
                .to_string(),
                published_at: release.published_at,
                architecture: "x86_64".to_string(),
                download_url: Some(asset.browser_download_url.clone()),
                checksum_algorithm,
                checksum,
                notes_url: Some(release.html_url),
            })
        })
        .collect();
    Ok(catalog(
        tool_id,
        "RubyInstaller for Windows",
        SOURCE,
        versions,
    ))
}

async fn fetch_php(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://windows.php.net/downloads/releases/";
    let html = checked_text(client, SOURCE).await?;
    let checksums = checked_text(
        client,
        "https://windows.php.net/downloads/releases/sha256sum.txt",
    )
    .await?;
    let checksum_map = checksums
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let checksum = parts.next()?;
            let filename = parts.next()?.trim_start_matches('*');
            Some((filename.to_ascii_lowercase(), checksum.to_string()))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let file_pattern =
        Regex::new(r#"href="(php-([0-9]+\.[0-9]+\.[0-9]+)-nts-Win32-vs[0-9]+-x64\.zip)""#)
            .map_err(|error| AppError::Message(error.to_string()))?;
    let versions = file_pattern
        .captures_iter(&html)
        .filter_map(|capture| {
            let filename = capture.get(1)?.as_str();
            let version = capture.get(2)?.as_str();
            let checksum = checksum_map.get(&filename.to_ascii_lowercase()).cloned();
            Some(RemoteVersion {
                version: version.to_string(),
                channel: "stable NTS".to_string(),
                published_at: None,
                architecture: "x86_64".to_string(),
                download_url: Some(format!("{SOURCE}{filename}")),
                checksum_algorithm: checksum.as_ref().map(|_| "sha256".to_string()),
                checksum,
                notes_url: Some(format!("https://www.php.net/releases/{version}/en.php")),
            })
        })
        .collect();
    Ok(catalog(tool_id, "PHP for Windows", SOURCE, versions))
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
    prerelease: bool,
    draft: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

type AssetSelector = fn(&[GithubAsset]) -> Option<&GithubAsset>;

async fn fetch_github_release(
    client: &reqwest::Client,
    tool_id: &str,
    source_name: &str,
    source_url: &str,
    selector: AssetSelector,
) -> AppResult<VersionCatalog> {
    let releases = checked_github_releases(client, source_url).await?;
    let versions = releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let asset = selector(&release.assets)?;
            let (checksum_algorithm, checksum) = asset
                .digest
                .as_deref()
                .and_then(|digest| digest.split_once(':'))
                .map(|(algorithm, value)| {
                    (
                        Some(algorithm.to_ascii_lowercase()),
                        Some(value.to_string()),
                    )
                })
                .unwrap_or((None, None));
            Some(RemoteVersion {
                version: release.tag_name.trim_start_matches('v').to_string(),
                channel: if release.prerelease {
                    "preview"
                } else {
                    "stable"
                }
                .to_string(),
                published_at: release.published_at,
                architecture: "x86_64".to_string(),
                download_url: Some(asset.browser_download_url.clone()),
                checksum_algorithm,
                checksum,
                notes_url: Some(release.html_url),
            })
        })
        .collect();
    Ok(catalog(tool_id, source_name, source_url, versions))
}

async fn checked_github_releases(
    client: &reqwest::Client,
    source_url: &str,
) -> AppResult<Vec<GithubRelease>> {
    let separator = if source_url.contains('?') { '&' } else { '?' };
    let mut releases = Vec::new();
    let mut page = 1_u32;
    loop {
        let url = format!("{source_url}{separator}per_page=100&page={page}");
        let mut current_page = match checked_json::<Vec<GithubRelease>>(client, &url).await {
            Ok(releases) => releases,
            Err(error) if github_api_access_limited(&error) => {
                // Keep already fetched history if the shared API quota runs out midway.
                if !releases.is_empty() {
                    return Ok(releases);
                }
                let Some(repository) = official_github_repository(source_url) else {
                    return Err(error);
                };
                return match fetch_github_web_latest(client, repository).await {
                    Ok(release) => Ok(vec![release]),
                    Err(fallback_error) => Err(AppError::Message(format!(
                        "{error}；官方发布页备用查询也失败：{fallback_error}"
                    ))),
                };
            }
            Err(error) => return Err(error),
        };
        let page_len = current_page.len();
        releases.append(&mut current_page);
        if page_len < 100 {
            break;
        }
        page += 1;
    }
    Ok(releases)
}

fn github_api_access_limited(error: &AppError) -> bool {
    matches!(error, AppError::SourceHttp { status: 403 | 429, .. })
}

fn official_github_repository(source_url: &str) -> Option<&'static str> {
    match source_url {
        "https://api.github.com/repos/git-for-windows/git/releases" => Some("git-for-windows/git"),
        "https://api.github.com/repos/Kitware/CMake/releases" => Some("Kitware/CMake"),
        "https://api.github.com/repos/oneclick/rubyinstaller2/releases" => Some("oneclick/rubyinstaller2"),
        _ => None,
    }
}

async fn fetch_github_web_latest(client: &reqwest::Client, repository: &str) -> AppResult<GithubRelease> {
    let latest_url = format!("https://github.com/{repository}/releases/latest");
    let response = checked_response(client, &latest_url, "text/html").await?;
    let release_url = response.url().clone();
    let tag = official_github_release_tag(repository, &release_url)?;
    // Only the redirect is needed. The expanded-assets fragment is much smaller
    // than downloading the release page and does not consume the REST API quota.
    drop(response);
    let assets_url = format!("https://github.com/{repository}/releases/expanded_assets/{tag}");
    let html = checked_text(client, &assets_url).await?;
    parse_github_web_release(repository, &tag, release_url.as_str(), &html)
}

fn official_github_release_tag(repository: &str, release_url: &url::Url) -> AppResult<String> {
    let prefix = format!("/{repository}/releases/tag/");
    let tag = release_url.path().strip_prefix(&prefix).unwrap_or_default();
    if release_url.scheme() != "https"
        || release_url.host_str() != Some("github.com")
        || release_url.port_or_known_default() != Some(443)
        || !release_url.username().is_empty()
        || release_url.password().is_some()
        || release_url.query().is_some()
        || release_url.fragment().is_some()
        || tag.is_empty()
        || !tag.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
    {
        return Err(AppError::InvalidSource("GitHub 最新发布页跳转到了不匹配的官方仓库或版本".to_string()));
    }
    Ok(tag.to_string())
}

fn parse_github_web_release(repository: &str, tag: &str, release_url: &str, html: &str) -> AppResult<GithubRelease> {
    let rows = Regex::new(r"(?is)<li\b[^>]*>(.*?)</li>")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let links = Regex::new(r#"(?i)\bhref\s*=\s*["']([^"']+)["']"#)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let digests = Regex::new(r"(?i)\bsha256:([0-9a-f]{64})\b")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let base = url::Url::parse("https://github.com/")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let prefix = format!("/{repository}/releases/download/{tag}/");
    let mut assets = Vec::new();
    let mut seen = HashSet::new();
    for row in rows.captures_iter(html) {
        let body = row.get(1).map(|capture| capture.as_str()).unwrap_or_default();
        for link in links.captures_iter(body) {
            let Some(href) = link.get(1) else { continue; };
            let Ok(download_url) = base.join(href.as_str()) else { continue; };
            if download_url.scheme() != "https"
                || download_url.host_str() != Some("github.com")
                || download_url.port_or_known_default() != Some(443)
                || !download_url.username().is_empty()
                || download_url.password().is_some()
                || download_url.query().is_some()
                || download_url.fragment().is_some()
            {
                continue;
            }
            let Some(name) = download_url.path().strip_prefix(&prefix) else { continue; };
            if name.is_empty() || name.contains('/') || name.contains('%') || !seen.insert(name.to_string()) {
                continue;
            }
            let hashes = digests.captures_iter(body)
                .filter_map(|capture| capture.get(1).map(|value| value.as_str().to_ascii_lowercase()))
                .collect::<HashSet<_>>();
            if hashes.len() > 1 || (body.to_ascii_lowercase().contains("sha256:") && hashes.is_empty()) {
                return Err(AppError::InvalidSource(format!("GitHub 官方发布页中 {name} 的 SHA-256 不明确或无效")));
            }
            assets.push(GithubAsset {
                name: name.to_string(),
                browser_download_url: download_url.to_string(),
                digest: hashes.into_iter().next().map(|hash| format!("sha256:{hash}")),
            });
        }
    }
    if assets.is_empty() {
        return Err(AppError::InvalidSource("GitHub 官方发布页没有返回该仓库的可下载发行包".to_string()));
    }
    Ok(GithubRelease {
        tag_name: tag.to_string(),
        html_url: release_url.to_string(),
        published_at: None,
        prerelease: false,
        draft: false,
        assets,
    })
}

fn select_portable_git(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    assets.iter().find(|asset| {
        asset.name.starts_with("PortableGit-")
            && asset.name.ends_with("-64-bit.7z.exe")
            && !asset.name.contains("arm64")
    })
}

fn select_cmake_zip(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    assets.iter().find(|asset| {
        asset.name.starts_with("cmake-") && asset.name.ends_with("-windows-x86_64.zip")
    })
}

fn select_ruby_archive(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    assets.iter().find(|asset| {
        asset.name.starts_with("rubyinstaller-")
            && asset.name.ends_with("-x64.7z")
            && !asset.name.contains("devkit")
    })
}

#[derive(Debug, Clone, Copy)]
enum AndroidPackage {
    CommandLineTools,
    Ndk,
    PlatformTools,
}

async fn fetch_android(
    client: &reqwest::Client,
    tool_id: &str,
    package: AndroidPackage,
) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://dl.google.com/android/repository/repository2-3.xml";
    let xml = checked_text(client, SOURCE).await?;
    let versions = parse_android_repository(&xml, package)?;
    Ok(catalog(
        tool_id,
        "Google Android Repository",
        SOURCE,
        versions,
    ))
}

fn parse_android_repository(xml: &str, package: AndroidPackage) -> AppResult<Vec<RemoteVersion>> {
    let package_pattern = Regex::new(r#"(?s)<remotePackage path="([^"]+)">(.*?)</remotePackage>"#)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let archive_pattern = Regex::new(r"(?s)<archive>(.*?)</archive>")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let url_pattern =
        Regex::new(r"<url>([^<]+)</url>").map_err(|error| AppError::Message(error.to_string()))?;
    let checksum_pattern = Regex::new(r#"<checksum type="([^"]+)">([^<]+)</checksum>"#)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let revision_pattern = Regex::new(r"(?s)<revision>(.*?)</revision>")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let component_pattern = Regex::new(r"<(major|minor|micro|preview)>([^<]+)</[^>]+>")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let mut versions = Vec::new();
    let mut seen = HashSet::new();

    for captures in package_pattern.captures_iter(xml) {
        let path = captures
            .get(1)
            .map(|value| value.as_str())
            .unwrap_or_default();
        let body = captures
            .get(2)
            .map(|value| value.as_str())
            .unwrap_or_default();
        let matches_package = match package {
            AndroidPackage::CommandLineTools => path.starts_with("cmdline-tools;"),
            AndroidPackage::Ndk => path.starts_with("ndk;"),
            AndroidPackage::PlatformTools => path == "platform-tools",
        };
        if !matches_package {
            continue;
        }
        let Some(archive) = archive_pattern
            .captures_iter(body)
            .filter_map(|archive| archive.get(1).map(|value| value.as_str()))
            .find(|archive| archive.contains("<host-os>windows</host-os>"))
        else {
            continue;
        };
        let Some(url) = url_pattern
            .captures(archive)
            .and_then(|capture| capture.get(1))
            .map(|value| value.as_str())
        else {
            continue;
        };
        let revision_body = revision_pattern
            .captures(body)
            .and_then(|capture| capture.get(1))
            .map(|value| value.as_str())
            .unwrap_or_default();
        let parts = component_pattern
            .captures_iter(revision_body)
            .filter_map(|capture| capture.get(2).map(|value| value.as_str().to_string()))
            .collect::<Vec<_>>();
        let version = if parts.is_empty() {
            path.split_once(';')
                .map(|(_, version)| version.to_string())
                .unwrap_or_else(|| path.to_string())
        } else {
            parts.join(".")
        };
        if !seen.insert(version.clone()) {
            continue;
        }
        let checksum = checksum_pattern.captures(archive);
        let checksum_algorithm = checksum
            .as_ref()
            .and_then(|capture| capture.get(1))
            .map(|value| value.as_str().to_ascii_lowercase());
        let checksum = checksum
            .and_then(|capture| capture.get(2))
            .map(|value| value.as_str().to_string());
        versions.push(RemoteVersion {
            version,
            channel: if body.contains("<preview>") || body.contains(r#"channelRef ref="channel-2""#)
            {
                "preview".to_string()
            } else {
                "stable".to_string()
            },
            published_at: None,
            architecture: "x86_64".to_string(),
            download_url: Some(format!("https://dl.google.com/android/repository/{url}")),
            checksum_algorithm,
            checksum,
            notes_url: Some("https://developer.android.com/studio/releases".to_string()),
        });
    }
    Ok(versions)
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        sync::{Arc, Mutex, mpsc},
    };

    struct MockSource {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: mpsc::Sender<()>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl MockSource {
        fn new(responses: Vec<String>) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let server_requests = requests.clone();
            let (stop, stopped) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut index = 0;
                while matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
                            let mut request = [0_u8; 8192];
                            let length = stream.read(&mut request).unwrap();
                            server_requests.lock().unwrap().push(String::from_utf8_lossy(&request[..length]).to_string());
                            let response = &responses[index.min(responses.len() - 1)];
                            let _ = stream.write_all(response.as_bytes());
                            index += 1;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => panic!("mock source accept: {error}"),
                    }
                }
            });
            Self { url, requests, stop, worker: Some(worker) }
        }

        fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    impl Drop for MockSource {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    fn http_response(status: &str, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    fn local_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(3)).build().unwrap()
    }

    #[derive(Debug, Deserialize)]
    struct RetryFixture {
        value: String,
    }

    #[test]
    fn github_rate_limit_stops_after_one_request_and_reports_reset_time() {
        let source = MockSource::new(vec![http_response(
            "403 Forbidden", "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 1792022400\r\n", r#"{"message":"API rate limit exceeded"}"#,
        )]);
        let error = tokio::runtime::Runtime::new().unwrap().block_on(checked_json::<RetryFixture>(&local_client(), &source.url)).unwrap_err();
        assert_eq!(source.request_count(), 1);
        assert!(matches!(error, AppError::SourceHttp { status: 403, rate_limited: true, .. }));
        let message = error.to_string();
        assert!(message.contains("请求限额已用尽"));
        assert!(message.contains("UTC"));
        assert!(!message.contains("无法识别的数据"));
    }

    #[test]
    fn text_rate_limit_and_missing_json_are_not_retried() {
        let source = MockSource::new(vec![http_response("429 Too Many Requests", "Retry-After: 120\r\n", "slow down")]);
        let error = tokio::runtime::Runtime::new().unwrap().block_on(checked_text(&local_client(), &source.url)).unwrap_err();
        assert!(matches!(error, AppError::SourceHttp { status: 429, rate_limited: true, retry_at: Some(_), .. }));
        assert_eq!(source.request_count(), 1);
        let missing = MockSource::new(vec![http_response("404 Not Found", "", "missing")]);
        let error = tokio::runtime::Runtime::new().unwrap().block_on(checked_json::<RetryFixture>(&local_client(), &missing.url)).unwrap_err();
        assert!(matches!(error, AppError::SourceHttp { status: 404, .. }));
        assert_eq!(missing.request_count(), 1);
    }

    #[test]
    fn source_retry_time_accepts_http_date_and_ignores_overflow() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "Wed, 14 Oct 2026 16:00:00 GMT".parse().unwrap());
        assert_eq!(source_retry_time(&headers, Utc::now()).unwrap().to_rfc3339(), "2026-10-14T16:00:00+00:00");
        headers.insert(reqwest::header::RETRY_AFTER, i64::MAX.to_string().parse().unwrap());
        assert!(source_retry_time(&headers, Utc::now()).is_none());
    }

    #[test]
    fn temporary_source_failure_retries_and_schema_errors_do_not() {
        let source = MockSource::new(vec![http_response("503 Service Unavailable", "", "temporary"), http_response("200 OK", "Content-Type: application/json\r\n", r#"{"value":"ready"}"#)]);
        let parsed = tokio::runtime::Runtime::new().unwrap().block_on(checked_json::<RetryFixture>(&local_client(), &source.url)).unwrap();
        assert_eq!(parsed.value, "ready");
        assert_eq!(source.request_count(), 2);
        let malformed = MockSource::new(vec![http_response("200 OK", "Content-Type: application/json\r\n", r#"{"wrong":true}"#)]);
        let error = tokio::runtime::Runtime::new().unwrap().block_on(checked_json::<RetryFixture>(&local_client(), &malformed.url)).unwrap_err();
        assert!(matches!(error, AppError::InvalidSource(_)));
        assert_eq!(malformed.request_count(), 1);
    }

    #[test]
    fn server_retry_after_does_not_trigger_an_immediate_retry() {
        let source = MockSource::new(vec![http_response("503 Service Unavailable", "Retry-After: 120\r\n", "temporary")]);
        let error = tokio::runtime::Runtime::new().unwrap().block_on(checked_text(&local_client(), &source.url)).unwrap_err();
        assert!(matches!(error, AppError::SourceHttp { status: 503, retry_at: Some(_), .. }));
        assert_eq!(source.request_count(), 1);
    }

    #[test]
    fn github_history_remains_available_when_a_later_page_hits_the_limit() {
        let release = serde_json::json!({
            "tag_name": "v2.50.1", "html_url": "https://github.com/git-for-windows/git/releases/tag/v2.50.1",
            "published_at": null, "prerelease": false, "draft": false, "assets": []
        });
        let page = serde_json::to_string(&vec![release; 100]).unwrap();
        let source = MockSource::new(vec![http_response("200 OK", "Content-Type: application/json\r\n", &page), http_response("403 Forbidden", "X-RateLimit-Remaining: 0\r\n", "limited")]);
        let releases = tokio::runtime::Runtime::new().unwrap().block_on(checked_github_releases(&local_client(), &source.url)).unwrap();
        assert_eq!(releases.len(), 100);
        assert_eq!(source.request_count(), 2);
        let requests = source.requests.lock().unwrap();
        assert!(requests[0].contains("per_page=100&page=1"));
        assert!(requests[1].contains("per_page=100&page=2"));
    }

    fn node_release(version: &str) -> NodeRelease {
        NodeRelease { version: version.to_string(), date: "2026-10-14".to_string(), files: vec!["win-x64-zip".to_string()], lts: serde_json::Value::Bool(false) }
    }

    #[test]
    fn node_history_is_complete_and_selected_checksum_is_fetched_once() {
        let versions = build_node_versions((0..80).map(|index| node_release(&format!("v20.0.{index}"))).collect());
        assert_eq!(versions.len(), 80);
        assert!(versions.iter().all(|version| version.checksum.is_none()));
        let mut selected = versions[75].clone();
        let filename = format!("node-v{}-win-x64.zip", selected.version);
        let checksum = "a".repeat(64);
        let source = MockSource::new(vec![http_response("200 OK", "", &format!("{}  unrelated.zip\n{checksum}  {filename}\n", "b".repeat(64)))]);
        tokio::runtime::Runtime::new().unwrap().block_on(fill_node_checksum(&local_client(), &mut selected, &source.url)).unwrap();
        assert_eq!(source.request_count(), 1);
        assert_eq!(selected.checksum.as_deref(), Some(checksum.as_str()));
        assert_eq!(selected.checksum_algorithm.as_deref(), Some("sha256"));
    }

    #[test]
    fn selected_node_metadata_rejects_missing_invalid_or_ambiguous_checksums() {
        for body in ["abcd  node-v20.0.1-win-x64.zip\n".to_string(), format!("{}  node-v20.0.1-win-x64.zip\n{}  node-v20.0.1-win-x64.zip\n", "a".repeat(64), "b".repeat(64)), format!("{}  node-v20.0.2-win-x64.zip\n", "a".repeat(64))] {
            let mut selected = build_node_versions(vec![node_release("v20.0.1")]).remove(0);
            let source = MockSource::new(vec![http_response("200 OK", "", &body)]);
            assert!(tokio::runtime::Runtime::new().unwrap().block_on(fill_node_checksum(&local_client(), &mut selected, &source.url)).is_err());
            assert!(selected.checksum.is_none());
        }
    }

    #[test]
    fn selected_node_metadata_reuses_valid_checksum_and_rejects_untrusted_url() {
        let mut selected = build_node_versions(vec![node_release("v20.0.1")]).remove(0);
        selected.checksum_algorithm = Some("sha256".to_string());
        selected.checksum = Some("a".repeat(64));
        tokio::runtime::Runtime::new().unwrap().block_on(ensure_install_metadata(&local_client(), "node", &mut selected)).unwrap();
        selected.download_url = Some("https://nodejs.org.evil.invalid/dist/node.zip".to_string());
        assert!(tokio::runtime::Runtime::new().unwrap().block_on(ensure_install_metadata(&local_client(), "node", &mut selected)).is_err());
    }

    #[test]
    fn github_web_fallback_keeps_official_asset_digests_and_rejects_foreign_links() {
        let checksum = "a".repeat(64);
        for (repository, tag, filename, selector) in [
            ("git-for-windows/git", "v2.50.1.windows.1", "PortableGit-2.50.1-64-bit.7z.exe", select_portable_git as AssetSelector),
            ("Kitware/CMake", "v4.0.1", "cmake-4.0.1-windows-x86_64.zip", select_cmake_zip as AssetSelector),
            ("oneclick/rubyinstaller2", "RubyInstaller-3.4.1-1", "rubyinstaller-3.4.1-1-x64.7z", select_ruby_archive as AssetSelector),
        ] {
            let fixture = format!(r#"<ul><li><a href="/{repository}/releases/download/{tag}/{filename}">download</a><clipboard-copy value="sha256:{checksum}"></clipboard-copy></li><li><a href="https://evil.invalid/{filename}">fake</a></li><li><a href="/someone/else/releases/download/{tag}/{filename}">fake</a></li></ul>"#);
            let page = format!("https://github.com/{repository}/releases/tag/{tag}");
            let release = parse_github_web_release(repository, tag, &page, &fixture).unwrap();
            assert_eq!(release.assets.len(), 1);
            let selected = selector(&release.assets).unwrap();
            assert_eq!(selected.name, filename);
            assert_eq!(selected.digest.as_deref(), Some(format!("sha256:{checksum}").as_str()));
            assert_eq!(official_github_release_tag(repository, &url::Url::parse(&page).unwrap()).unwrap(), tag);
            assert!(official_github_release_tag(repository, &url::Url::parse(&page.replace("github.com", "github.com.evil.invalid")).unwrap()).is_err());
        }
    }

    #[test]
    fn github_web_fallback_rejects_conflicting_digests() {
        let fixture = format!(r#"<li><a href="/git-for-windows/git/releases/download/v2.50.1/PortableGit-2.50.1-64-bit.7z.exe">download</a>sha256:{} sha256:{}</li>"#, "a".repeat(64), "b".repeat(64));
        assert!(parse_github_web_release("git-for-windows/git", "v2.50.1", "https://github.com/git-for-windows/git/releases/tag/v2.50.1", &fixture).is_err());
    }

    #[test]
    fn retries_when_a_json_response_body_is_truncated() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for body in [r#"{"value":"#, r#"{"value":"complete"}"#] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let parsed = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(checked_json::<RetryFixture>(
                &client,
                &format!("http://{address}/catalog"),
            ))
            .unwrap();
        server.join().unwrap();
        assert_eq!(parsed.value, "complete");
    }

    #[test]
    fn parses_windows_android_package_and_sha1() {
        let fixture = r#"
          <remotePackage path="platform-tools">
            <revision><major>37</major><minor>0</minor><micro>0</micro></revision>
            <channelRef ref="channel-0"/>
            <archives>
              <archive><complete><checksum type="sha1">linux</checksum><url>linux.zip</url></complete><host-os>linux</host-os></archive>
              <archive><complete><checksum type="sha1">abc123</checksum><url>platform-tools-win.zip</url></complete><host-os>windows</host-os></archive>
            </archives>
          </remotePackage>
        "#;
        let versions = parse_android_repository(fixture, AndroidPackage::PlatformTools).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "37.0.0");
        assert_eq!(versions[0].checksum.as_deref(), Some("abc123"));
        assert_eq!(versions[0].checksum_algorithm.as_deref(), Some("sha1"));
        assert!(
            versions[0]
                .download_url
                .as_deref()
                .unwrap()
                .ends_with("platform-tools-win.zip")
        );
    }

    #[test]
    fn python_catalog_keeps_every_installable_x64_release() {
        let releases = (0..40)
            .map(|index| PythonRelease {
                name: format!("Python 3.{}.{}", 10 + index / 10, index % 10),
                release_date: format!("2026-{:02}-{:02}", 1 + index / 28, 1 + index % 28),
                pre_release: false,
                release_notes_url: None,
                resource_uri: format!("https://www.python.org/api/v2/downloads/release/{index}/"),
            })
            .collect::<Vec<_>>();
        let files = (0..40)
            .map(|index| PythonReleaseFile {
                name: "Windows embeddable package (64-bit)".to_string(),
                release: format!("https://www.python.org/api/v2/downloads/release/{index}/"),
                url: format!("https://www.python.org/ftp/python/{index}/python.zip"),
                sha256_sum: format!("{index:064x}"),
            })
            .collect::<Vec<_>>();

        let versions = build_python_versions(releases, files);

        assert_eq!(versions.len(), 40);
        assert!(
            versions
                .iter()
                .all(|version| version.architecture == "x86_64")
        );
    }

    #[test]
    #[ignore = "requires live official services"]
    fn live_official_catalogs_return_windows_downloads() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let providers = vec![
                ("python", VersionSourceKind::Python),
                ("java", VersionSourceKind::Adoptium),
                ("go", VersionSourceKind::Go),
                ("rust", VersionSourceKind::Rust),
                ("node", VersionSourceKind::Node),
                ("git", VersionSourceKind::GitForWindows),
                ("android-sdk", VersionSourceKind::AndroidSdk),
                ("android-ndk", VersionSourceKind::AndroidNdk),
                ("gradle", VersionSourceKind::Gradle),
                ("cmake", VersionSourceKind::CMake),
                ("adb", VersionSourceKind::Adb),
                ("maven", VersionSourceKind::Maven),
                ("dotnet", VersionSourceKind::DotNet),
                ("ruby", VersionSourceKind::Ruby),
                ("php", VersionSourceKind::Php),
            ];
            assert_live_catalogs(providers).await;
        });
    }

    #[test]
    #[ignore = "requires live official services"]
    fn live_added_catalogs_return_windows_downloads() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert_live_catalogs(vec![
                ("maven", VersionSourceKind::Maven),
                ("dotnet", VersionSourceKind::DotNet),
                ("ruby", VersionSourceKind::Ruby),
                ("php", VersionSourceKind::Php),
            ])
            .await;
        });
    }

    async fn assert_live_catalogs(providers: Vec<(&'static str, VersionSourceKind)>) {
        let client = reqwest::Client::builder()
            .user_agent("EnvNexus-AI-source-smoke-test/0.1.0")
            .https_only(true)
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .unwrap();
        let results = stream::iter(providers)
            .map(|(tool_id, source)| {
                let client = client.clone();
                async move { (tool_id, fetch(&client, tool_id, source).await) }
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
        let mut failures = Vec::new();
        for (tool_id, result) in results {
            match result {
                Ok(catalog)
                    if !catalog.versions.is_empty()
                        && catalog.versions.iter().any(|version| {
                            version
                                .download_url
                                .as_deref()
                                .is_some_and(|url| url.starts_with("https://"))
                        }) =>
                {
                    println!("{tool_id}: {} Windows downloads", catalog.versions.len());
                }
                Ok(catalog) if catalog.versions.is_empty() => {
                    failures.push(format!("{tool_id}: returned an empty catalog"));
                }
                Ok(_) => failures.push(format!("{tool_id}: returned no HTTPS Windows download")),
                Err(error) => failures.push(format!("{tool_id}: {error}")),
            }
        }
        assert!(
            failures.is_empty(),
            "official source failures:\n{}",
            failures.join("\n")
        );
    }
}
