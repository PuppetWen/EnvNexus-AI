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

const PARTIAL_CATALOG_SUFFIX: &str = "（官方源部分结果）";

pub(crate) fn is_partial_catalog(catalog: &VersionCatalog) -> bool {
    catalog.source_name.ends_with(PARTIAL_CATALOG_SUFFIX)
}

fn github_catalog(
    tool_id: &str,
    source_name: &str,
    source_url: &str,
    partial: bool,
    versions: Vec<RemoteVersion>,
) -> VersionCatalog {
    let source_name = if partial {
        format!("{source_name}{PARTIAL_CATALOG_SUFFIX}")
    } else {
        source_name.to_string()
    };
    catalog(tool_id, &source_name, source_url, versions)
}

async fn checked_text(client: &reqwest::Client, url: &str) -> AppResult<String> {
    for attempt in 0..3 {
        let (error, retryable) =
            match checked_response(client, url, "text/plain, text/html;q=0.9, */*;q=0.8").await {
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
                    Ok(bytes) if bytes.is_empty() => {
                        (AppError::InvalidSource(format!("{url}：响应体为空")), true)
                    }
                    Ok(bytes) => match serde_json::from_slice::<T>(&bytes) {
                        Ok(value) => return Ok(value),
                        Err(error) => {
                            let retryable = error.is_eof();
                            (
                                AppError::InvalidSource(format!(
                                    "{url}：JSON 数据不完整或格式已变化（{content_type}）：{error}"
                                )),
                                retryable,
                            )
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
    checked_source_response(response)
}

fn checked_source_response(response: reqwest::Response) -> AppResult<reqwest::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let headers = response.headers();
    let rate_limited = status == 429
        || (status == 403
            && (headers
                .get("x-ratelimit-remaining")
                .is_some_and(|value| value == "0")
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
    Err(AppError::SourceHttp {
        status,
        url: safe_url.to_string(),
        retry_at,
        rate_limited,
    })
}

fn source_retry_time(
    headers: &reqwest::header::HeaderMap,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if let Some(value) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(retry_at) = value
            .trim()
            .parse::<i64>()
            .ok()
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
    headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
}

fn retryable_source_error(error: &AppError) -> bool {
    match error {
        AppError::Http(error) => error.is_timeout() || error.is_connect() || error.is_body(),
        AppError::SourceHttp {
            status,
            rate_limited: false,
            retry_at,
            ..
        } => {
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

const PYTHON_RUNTIME_SOURCE: &str = "https://www.python.org/ftp/python/index-windows.json";
const PYTHON_RELEASE_SOURCE: &str =
    "https://www.python.org/api/v2/downloads/release/?is_published=true";
const PYTHON_FILE_SOURCE: &str = "https://www.python.org/api/v2/downloads/release_file/?os=1";
const PYTHON_LEGACY_SOURCE: &str = "https://www.python.org/ftp/python/src/";
const PYTHON_EARLY_SOURCE: &str = "https://www.python.org/download/releases/";
const PYTHON_FTP_SOURCE: &str = "https://www.python.org/ftp/python/";
const PYTHON_TAGS_SOURCE: &str =
    "https://github.com/python/cpython.git/info/refs?service=git-upload-pack";
const PYTHON_TAGS_UPLOAD: &str = "https://github.com/python/cpython.git/git-upload-pack";
const PYTHON_TAGS_REQUEST: &[u8] = b"0014command=ls-refs\n0001001bref-prefix refs/tags/v\n0000";
const PYTHON_TAGS_MAX_BYTES: usize = 32 * 1024 * 1024;
const PYTHON_HISTORY_NAME: &str = "Python.org 发布与历史归档（含预览版）";

#[derive(Debug, Deserialize)]
struct PythonRelease {
    name: String,
    slug: String,
    version: u32,
    is_published: bool,
    pre_release: bool,
    release_date: Option<String>,
    resource_uri: String,
}

#[derive(Debug, Deserialize)]
struct PythonReleaseFile {
    name: String,
    os: String,
    release: String,
    is_source: bool,
    url: String,
    #[serde(default)]
    md5_sum: String,
    #[serde(default)]
    sha256_sum: String,
}

#[derive(Debug, Deserialize)]
struct PythonRuntimeIndex {
    versions: Vec<PythonRuntime>,
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PythonRuntime {
    schema: u32,
    id: String,
    company: String,
    tag: String,
    #[serde(rename = "sort-version")]
    version: String,
    url: String,
    hash: Option<HashMap<String, String>>,
}

async fn fetch_python(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    // Both API collections are unpaginated bulk responses. Never hydrate hundreds
    // of releases separately: python.org also limits anonymous API requests.
    let (releases, files, (runtimes, (legacy, ftp, early), tags)) =
        futures_util::future::try_join3(
            checked_json::<Vec<PythonRelease>>(client, PYTHON_RELEASE_SOURCE),
            checked_json::<Vec<PythonReleaseFile>>(client, PYTHON_FILE_SOURCE),
            futures_util::future::try_join3(
                fetch_python_indexes(|url| async move {
                    checked_json::<PythonRuntimeIndex>(client, &url).await
                }),
                futures_util::future::try_join3(
                    checked_text(client, PYTHON_LEGACY_SOURCE),
                    checked_text(client, PYTHON_FTP_SOURCE),
                    checked_text(client, PYTHON_EARLY_SOURCE),
                ),
                fetch_python_source_tags(client),
            ),
        )
        .await?;
    if releases.is_empty() || files.is_empty() || runtimes.is_empty() {
        return Err(AppError::InvalidSource(
            "Python 官方历史目录或完整运行时索引为空，不能作为完整版本列表".into(),
        ));
    }
    let archives = format!("{legacy}\n{early}");
    let mut versions = build_python_history(releases, files, runtimes, &archives, &ftp)?;
    merge_python_source_tags(&mut versions, tags);
    Ok(catalog(
        tool_id,
        PYTHON_HISTORY_NAME,
        PYTHON_RELEASE_SOURCE,
        versions,
    ))
}

async fn fetch_python_source_tags(client: &reqwest::Client) -> AppResult<Vec<(String, String)>> {
    fetch_python_source_tags_from(client, PYTHON_TAGS_SOURCE, PYTHON_TAGS_UPLOAD).await
}

async fn fetch_python_source_tags_from(
    client: &reqwest::Client,
    discovery_url: &str,
    upload_url: &str,
) -> AppResult<Vec<(String, String)>> {
    // The two v2 requests avoid downloading tens of thousands of pull refs.
    let response = client
        .get(discovery_url)
        .header("Git-Protocol", "version=2")
        .header(
            reqwest::header::ACCEPT,
            "application/x-git-upload-pack-advertisement",
        )
        .send()
        .await?;
    let capabilities =
        read_python_git_response(response, "application/x-git-upload-pack-advertisement").await?;
    parse_python_git_capabilities(&capabilities)?;
    let response = client
        .post(upload_url)
        .header("Git-Protocol", "version=2")
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-git-upload-pack-request",
        )
        .header(
            reqwest::header::ACCEPT,
            "application/x-git-upload-pack-result",
        )
        .body(PYTHON_TAGS_REQUEST)
        .send()
        .await?;
    let tags = read_python_git_response(response, "application/x-git-upload-pack-result").await?;
    parse_python_ls_refs(&tags)
}

async fn read_python_git_response(
    response: reqwest::Response,
    expected_type: &str,
) -> AppResult<Vec<u8>> {
    let response = checked_source_response(response)?;
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|content_type| content_type.trim().eq_ignore_ascii_case(expected_type))
    {
        return Err(AppError::InvalidSource(
            "CPython 官方源码标签服务返回了错误的 Content-Type".into(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > PYTHON_TAGS_MAX_BYTES as u64)
    {
        return Err(AppError::InvalidSource(
            "CPython 官方源码标签响应超过 32 MiB 上限".into(),
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > PYTHON_TAGS_MAX_BYTES - bytes.len() {
            return Err(AppError::InvalidSource(
                "CPython 官方源码标签响应超过 32 MiB 上限".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn parse_python_git_capabilities(bytes: &[u8]) -> AppResult<()> {
    let invalid = || {
        AppError::InvalidSource("CPython 官方源码标签服务未提供完整的 Git v2 ls-refs 能力".into())
    };
    if bytes.len() > PYTHON_TAGS_MAX_BYTES {
        return Err(invalid());
    }
    let mut offset = 0;
    let mut first = python_git_packet(bytes, &mut offset)?;
    // GitHub's smart HTTP advertisement includes the service announcement.
    if first == Some(b"# service=git-upload-pack\n".as_slice()) {
        if python_git_packet(bytes, &mut offset)?.is_some() {
            return Err(invalid());
        }
        first = python_git_packet(bytes, &mut offset)?;
    }
    if first != Some(b"version 2\n".as_slice()) {
        return Err(invalid());
    }
    let mut ls_refs = false;
    let mut complete = false;
    let mut seen = HashSet::new();
    while offset < bytes.len() {
        let Some(packet) = python_git_packet(bytes, &mut offset)? else {
            complete = offset == bytes.len();
            break;
        };
        let capability = std::str::from_utf8(packet)
            .ok()
            .and_then(|line| line.strip_suffix('\n'))
            .ok_or_else(invalid)?;
        let name = capability.split('=').next().ok_or_else(invalid)?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || !capability
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic())
            || !seen.insert(name)
        {
            return Err(invalid());
        }
        ls_refs |= name == "ls-refs";
    }
    if complete && ls_refs {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn parse_python_ls_refs(bytes: &[u8]) -> AppResult<Vec<(String, String)>> {
    let invalid =
        || AppError::InvalidSource("CPython 官方源码标签不是完整有效的 Git v2 ls-refs".into());
    if bytes.len() > PYTHON_TAGS_MAX_BYTES {
        return Err(invalid());
    }
    let mut offset = 0;
    let mut complete = false;
    let mut tags = HashMap::<String, (String, String)>::new();
    while offset < bytes.len() {
        let Some(packet) = python_git_packet(bytes, &mut offset)? else {
            complete = offset == bytes.len();
            break;
        };
        let record = std::str::from_utf8(packet)
            .ok()
            .and_then(|line| line.strip_suffix('\n'))
            .ok_or_else(invalid)?;
        let (object, reference) = record.split_once(' ').ok_or_else(invalid)?;
        let raw = reference.strip_prefix("refs/tags/v").ok_or_else(invalid)?;
        let version = normalize_python_history_version(raw).ok_or_else(invalid)?;
        if !matches!(object.len(), 40 | 64)
            || !object.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !valid_python_git_ref(reference)
        {
            return Err(invalid());
        }
        if let Some((_, previous)) = tags.get(raw)
            && previous != object
        {
            return Err(invalid());
        }
        tags.insert(raw.into(), (version, object.into()));
    }
    if !complete || tags.is_empty() {
        return Err(invalid());
    }
    let mut versions = tags
        .into_iter()
        .map(|(tag, (version, _))| (version, tag))
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(versions)
}

fn python_git_packet<'a>(bytes: &'a [u8], offset: &mut usize) -> AppResult<Option<&'a [u8]>> {
    let invalid = || AppError::InvalidSource("CPython 官方源码标签 pkt-line 长度无效".into());
    let header = bytes.get(*offset..*offset + 4).ok_or_else(invalid)?;
    if !header.iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid());
    }
    let length = usize::from_str_radix(std::str::from_utf8(header).map_err(|_| invalid())?, 16)
        .map_err(|_| invalid())?;
    *offset += 4;
    if length == 0 {
        return Ok(None);
    }
    // These discovery and ls-refs responses contain data and flush packets,
    // never command delimiters or response-end markers.
    if !(4..=65_520).contains(&length) {
        return Err(invalid());
    }
    let payload = bytes
        .get(*offset..*offset + length - 4)
        .ok_or_else(invalid)?;
    *offset += length - 4;
    Ok(Some(payload))
}

fn valid_python_git_ref(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.contains(['~', '^', ':', '?', '*', '[', '\\'])
        && !reference.contains("..")
        && !reference.contains("@{")
        && !reference
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        && reference.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && !part.ends_with(".lock")
        })
}

#[cfg(test)]
fn parse_python_source_tags(bytes: &[u8]) -> AppResult<Vec<(String, String)>> {
    let invalid =
        || AppError::InvalidSource("CPython 官方源码标签不是完整有效的 Git v0 refs".into());
    if bytes.len() > PYTHON_TAGS_MAX_BYTES {
        return Err(invalid());
    }
    let mut offset = 0;
    if python_git_packet(bytes, &mut offset)? != Some(b"# service=git-upload-pack\n".as_slice())
        || python_git_packet(bytes, &mut offset)?.is_some()
    {
        return Err(invalid());
    }
    let mut first_ref = true;
    let mut complete = false;
    let mut tags = HashMap::<String, (String, String)>::new();
    while offset < bytes.len() {
        let Some(packet) = python_git_packet(bytes, &mut offset)? else {
            complete = offset == bytes.len();
            break;
        };
        let record = std::str::from_utf8(packet)
            .ok()
            .and_then(|record| record.strip_suffix('\n'))
            .ok_or_else(invalid)?;
        let record = if let Some((reference, capabilities)) = record.split_once('\0') {
            if !first_ref
                || capabilities.is_empty()
                || !capabilities
                    .bytes()
                    .all(|byte| byte == b' ' || byte.is_ascii_graphic())
            {
                return Err(invalid());
            }
            reference
        } else {
            record
        };
        let (object, reference) = record.split_once(' ').ok_or_else(invalid)?;
        if !matches!(object.len(), 40 | 64) || !object.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        let peeled = reference.ends_with("^{}");
        let plain_ref = reference.strip_suffix("^{}").unwrap_or(reference);
        if !valid_python_git_ref(plain_ref) {
            return Err(invalid());
        }
        if plain_ref == "HEAD" {
            if !first_ref || peeled {
                return Err(invalid());
            }
        } else if plain_ref.starts_with("refs/heads/") {
            if peeled {
                return Err(invalid());
            }
        } else if let Some(pull) = plain_ref.strip_prefix("refs/pull/") {
            let (number, kind) = pull.split_once('/').ok_or_else(invalid)?;
            if peeled
                || number.is_empty()
                || !number.bytes().all(|byte| byte.is_ascii_digit())
                || !matches!(kind, "head" | "merge")
            {
                return Err(invalid());
            }
        } else if let Some(tag) = plain_ref.strip_prefix("refs/tags/") {
            if !peeled
                && let Some(raw) = tag.strip_prefix('v')
                && let Some(version) = normalize_python_history_version(raw)
            {
                if let Some((_, previous)) = tags.get(raw)
                    && previous != object
                {
                    return Err(invalid());
                }
                tags.insert(raw.into(), (version, object.into()));
            }
        } else {
            return Err(invalid());
        }
        first_ref = false;
    }
    if !complete || tags.is_empty() {
        return Err(invalid());
    }
    let mut versions = tags
        .into_iter()
        .map(|(tag, (version, _))| (version, tag))
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(versions)
}

fn merge_python_source_tags(versions: &mut Vec<RemoteVersion>, tags: Vec<(String, String)>) {
    let mut seen = versions
        .iter()
        .map(|remote| remote.version.clone())
        .collect::<HashSet<_>>();
    for (version, tag) in tags {
        if !seen.insert(version.clone()) {
            continue;
        }
        let mut remote = python_archive_row(
            version,
            format!("https://github.com/python/cpython/tree/v{tag}"),
            "官方 CPython 源码标签可查，发布数据库缺少可校验的 Windows 安装包元数据",
        );
        if remote.channel == "stable" {
            remote.channel = "archive".into();
        }
        versions.push(remote);
    }
}

fn python_source_tag_notes(version: &str, notes: &str) -> bool {
    let Ok(url) = url::Url::parse(notes) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url
            .path()
            .strip_prefix("/python/cpython/tree/v")
            .and_then(normalize_python_history_version)
            .as_deref()
            == Some(version)
        && notes == url.as_str()
}

fn build_python_history(
    releases: Vec<PythonRelease>,
    files: Vec<PythonReleaseFile>,
    runtimes: Vec<RemoteVersion>,
    legacy_html: &str,
    ftp_html: &str,
) -> AppResult<Vec<RemoteVersion>> {
    let mut files_by_release = HashMap::<String, Vec<PythonReleaseFile>>::new();
    for file in files {
        if file.os.ends_with("/os/1/") && !file.is_source {
            files_by_release
                .entry(file.release.clone())
                .or_default()
                .push(file);
        }
    }
    let mut versions = HashMap::<String, RemoteVersion>::new();
    let mut package_checksums = HashMap::new();
    for release in releases {
        if !release.is_published || !matches!(release.version, 2 | 3) {
            continue;
        }
        let version = release
            .name
            .strip_prefix("Python ")
            .and_then(normalize_python_history_version)
            .filter(|version| version.starts_with(&format!("{}.", release.version)))
            .ok_or_else(|| AppError::InvalidSource("Python 官方发布记录的版本号无效".into()))?;
        if release.slug.is_empty()
            || !release
                .slug
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(AppError::InvalidSource(
                "Python 官方发布记录的说明页地址无效".into(),
            ));
        }
        let mut remote = RemoteVersion {
            install_unavailable_reason: None,
            version: version.clone(),
            channel: if release.pre_release {
                "preview"
            } else {
                "stable"
            }
            .into(),
            published_at: release.release_date,
            architecture: "source".into(),
            download_url: None,
            checksum_algorithm: None,
            checksum: None,
            notes_url: Some(format!(
                "https://www.python.org/downloads/release/{}/",
                release.slug
            )),
        };
        remote.install_unavailable_reason =
            Some("此版本的官方发布记录未提供适合本机的完整 Windows x64/x86 安装包".into());
        let release_files = files_by_release
            .remove(&release.resource_uri)
            .unwrap_or_default();
        let mut candidates = Vec::new();
        for file in release_files {
            let Some((url, architecture)) = python_installer_file(&version, &file) else {
                continue;
            };
            if !python_supported_installer_format(&version, &url) {
                remote.install_unavailable_reason =
                    Some("官方完整安装包采用尚未支持的旧式 EXE 格式；可通过官网归档查看".into());
                continue;
            }
            let Some((algorithm, checksum)) = python_file_checksum(&file) else {
                remote.install_unavailable_reason =
                    Some("官方安装包未提供有效的 SHA256 或 MD5 校验值，暂不能安全安装".into());
                continue;
            };
            let key = (url.clone(), algorithm.clone());
            if let Some(previous) = package_checksums.insert(key, checksum.clone())
                && previous != checksum
            {
                return Err(AppError::InvalidSource(
                    "Python 官方目录对同一安装包给出了冲突的校验值".into(),
                ));
            }
            candidates.push((architecture, url, algorithm, checksum));
        }
        // The official database contains distinct release records for the same
        // version (for example 3.3.5rc1). Merge their package candidates instead
        // of rejecting the whole catalog or overwriting an installable row.
        if let Some(previous) = versions.remove(&version) {
            if let (Some(url), Some(algorithm), Some(checksum)) = (
                previous.download_url,
                previous.checksum_algorithm,
                previous.checksum,
            ) {
                candidates.push((previous.architecture, url, algorithm, checksum));
            }
            if remote.published_at.is_none() {
                remote.published_at = previous.published_at;
            }
        }
        candidates.sort_by(|left, right| {
            let rank = |candidate: &(String, String, String, String)| {
                (candidate.0 != "x86_64", candidate.2 != "sha256")
            };
            rank(left).cmp(&rank(right)).then(left.1.cmp(&right.1))
        });
        if let Some((architecture, url, algorithm, checksum)) = candidates.first() {
            if candidates.iter().skip(1).any(|candidate| {
                candidate.1 == *url && candidate.2 == *algorithm && candidate.3 != *checksum
            }) {
                return Err(AppError::InvalidSource(
                    "Python 官方目录对同一安装包给出了冲突的校验值".into(),
                ));
            }
            remote.architecture = architecture.clone();
            remote.download_url = Some(url.clone());
            remote.checksum_algorithm = Some(algorithm.clone());
            remote.checksum = Some(checksum.clone());
            remote.install_unavailable_reason = None;
        }
        versions.insert(version, remote);
    }
    if versions.is_empty() {
        return Err(AppError::InvalidSource(
            "Python 官方发布目录不含 Python 2/3".into(),
        ));
    }
    merge_python_archives(&mut versions, legacy_html, ftp_html)?;
    // Full PythonCore ZIPs retain their official SHA-256, and always take
    // precedence over the traditional installer of the same release.
    for mut runtime in runtimes {
        if let Some(release) = versions.get(&runtime.version) {
            runtime.published_at = release.published_at.clone();
            runtime.notes_url = release.notes_url.clone();
        }
        versions.insert(runtime.version.clone(), runtime);
    }
    Ok(versions.into_values().collect())
}

fn normalize_python_history_version(version: &str) -> Option<String> {
    let suffix_start = version
        .find(|character: char| character.is_ascii_alphabetic())
        .unwrap_or(version.len());
    let (base, suffix) = version.split_at(suffix_start);
    let parts = base.split('.').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len())
        || !matches!(parts[0], "0" | "1" | "2" | "3")
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let suffix = if suffix.is_empty() {
        String::new()
    } else {
        let (marker, number) = ["rc", "a", "b", "c"]
            .into_iter()
            .find_map(|marker| suffix.strip_prefix(marker).map(|number| (marker, number)))?;
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        format!("{}{number}", if marker == "c" { "rc" } else { marker })
    };
    Some(format!(
        "{base}{}{suffix}",
        if parts.len() == 2 { ".0" } else { "" }
    ))
}

fn python_release_base(version: &str) -> &str {
    version
        .split(|character: char| character.is_ascii_alphabetic())
        .next()
        .unwrap_or(version)
}

fn python_download_url(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str() != Some("www.python.org")
        || url.port_or_known_default() != Some(if url.scheme() == "http" { 80 } else { 443 })
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with("/ftp/python/")
        || url.path().contains('%')
    {
        return None;
    }
    if url.scheme() == "http" {
        url.set_port(None).ok()?;
        url.set_scheme("https").ok()?;
    }
    Some(url.to_string())
}

fn python_installer_architecture(version: &str, value: &str) -> Option<&'static str> {
    let url = url::Url::parse(value).ok()?;
    let relative = url.path().strip_prefix("/ftp/python/")?;
    let (directory, filename) = relative.split_once('/')?;
    let base = python_release_base(version);
    let short_base = base.strip_suffix(".0").unwrap_or(base);
    if directory != base && directory != short_base {
        return None;
    }
    let short_version = format!("{short_base}{}", &version[base.len()..]);
    for spelling in [version, short_version.as_str()] {
        for (suffix, architecture) in [
            ("-amd64.exe", "x86_64"),
            (".amd64.msi", "x86_64"),
            (".exe", "x86"),
            (".msi", "x86"),
            ("-1.exe", "x86"),
        ] {
            if filename.eq_ignore_ascii_case(&format!("python-{spelling}{suffix}")) {
                return Some(architecture);
            }
        }
    }
    None
}

fn python_installer_file(version: &str, file: &PythonReleaseFile) -> Option<(String, String)> {
    let name = file.name.to_ascii_lowercase();
    if !name.contains("installer")
        || ["web", "embed", "debug", "database"]
            .iter()
            .any(|excluded| name.contains(excluded))
    {
        return None;
    }
    let url = python_download_url(&file.url)?;
    let architecture = python_installer_architecture(version, &url)?;
    Some((url, architecture.to_string()))
}

fn python_supported_installer_format(version: &str, url: &str) -> bool {
    if url.to_ascii_lowercase().ends_with(".msi") {
        return true;
    }
    let parts = python_release_base(version).split('.').collect::<Vec<_>>();
    let minor = parts.get(1).and_then(|minor| minor.parse::<u32>().ok());
    (parts.first() == Some(&"3") && minor.is_some_and(|minor| minor >= 5))
        || (parts.first() == Some(&"2") && minor.is_some_and(|minor| minor <= 3))
}

fn python_file_checksum(file: &PythonReleaseFile) -> Option<(String, String)> {
    if !file.sha256_sum.is_empty() {
        return valid_sha256(&file.sha256_sum)
            .then(|| ("sha256".into(), file.sha256_sum.to_ascii_lowercase()));
    }
    valid_hex_digest(&file.md5_sum, 32).then(|| ("md5".into(), file.md5_sum.to_ascii_lowercase()))
}

fn merge_python_archives(
    versions: &mut HashMap<String, RemoteVersion>,
    legacy_html: &str,
    ftp_html: &str,
) -> AppResult<()> {
    let links = Regex::new(r#"(?i)\bhref\s*=\s*["']([^"']+)["']"#)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let early_links = Regex::new(
        r#"(?is)<a\b[^>]*\bhref\s*=\s*["']([^"']+)["'][^>]*>\s*Python\s+([^<\s]+)\s*</a>"#,
    )
    .map_err(|error| AppError::Message(error.to_string()))?;
    let mut early_count = 0;
    for capture in early_links.captures_iter(legacy_html) {
        let Some(version) = normalize_python_history_version(&capture[2]) else {
            continue;
        };
        let Some(notes) = url::Url::parse(PYTHON_EARLY_SOURCE)
            .ok()
            .and_then(|source| source.join(&capture[1]).ok())
            .filter(|notes| notes.as_str().starts_with(PYTHON_EARLY_SOURCE))
        else {
            continue;
        };
        early_count += 1;
        versions.entry(version.clone()).or_insert_with(|| {
            python_archive_row(
                version,
                notes.to_string(),
                "官方历史发布页可查，尚无可验证的 Windows 安装包元数据",
            )
        });
    }
    let mut legacy_count = 0;
    for capture in links.captures_iter(legacy_html) {
        let filename = &capture[1];
        let Some(stem) = filename
            .strip_suffix(".tar.gz")
            .or_else(|| filename.strip_suffix(".tgz"))
        else {
            continue;
        };
        let raw = match stem {
            "py152" => "1.5.2",
            "pyth151" => "1.5.1",
            stem => match stem
                .strip_prefix("python-")
                .or_else(|| stem.strip_prefix("python"))
                .or_else(|| stem.strip_prefix("Python-"))
            {
                Some(version) => version,
                None => continue,
            },
        };
        let Some(version) = normalize_python_history_version(raw) else {
            continue;
        };
        legacy_count += 1;
        versions.entry(version.clone()).or_insert_with(|| {
            python_archive_row(
                version,
                PYTHON_LEGACY_SOURCE.to_string(),
                "官方源码归档可查，尚无可验证的 Windows 安装包元数据",
            )
        });
    }
    let mut ftp_count = 0;
    for capture in links.captures_iter(ftp_html) {
        let Some(directory) = capture[1].strip_suffix('/') else {
            continue;
        };
        let Some(version) = normalize_python_history_version(directory) else {
            continue;
        };
        ftp_count += 1;
        // A directory also contains prereleases (e.g. 3.16.0a1). It is not
        // evidence that the final version has been published.
        if versions
            .keys()
            .any(|known| python_release_base(known) == version)
        {
            continue;
        }
        versions.entry(version.clone()).or_insert_with(|| {
            let mut row = python_archive_row(
                version,
                format!("{PYTHON_FTP_SOURCE}{directory}/"),
                "官方历史目录可查，发布数据库未提供可验证的 Windows 安装包元数据",
            );
            row.channel = "archive".into();
            row
        });
    }
    if legacy_count == 0 || ftp_count == 0 || early_count == 0 {
        return Err(AppError::InvalidSource(
            "Python 官方历史归档未返回可识别的版本目录".into(),
        ));
    }
    Ok(())
}

fn python_archive_row(version: String, notes_url: String, reason: &str) -> RemoteVersion {
    let preview = version.bytes().any(|byte| byte.is_ascii_alphabetic());
    let mut remote = RemoteVersion {
        install_unavailable_reason: None,
        version,
        channel: if preview { "preview" } else { "stable" }.into(),
        published_at: None,
        architecture: "source".into(),
        download_url: None,
        checksum_algorithm: None,
        checksum: None,
        notes_url: Some(notes_url),
    };
    remote.install_unavailable_reason = Some(reason.into());
    remote
}

async fn fetch_python_indexes<F, Fut>(mut fetch_index: F) -> AppResult<Vec<RemoteVersion>>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = AppResult<PythonRuntimeIndex>>,
{
    let mut next = Some(PYTHON_RUNTIME_SOURCE.to_string());
    let mut seen_pages = HashSet::new();
    let mut versions = HashMap::<String, RemoteVersion>::new();
    for _ in 0..16 {
        let Some(source) = next.take() else {
            return Ok(versions.into_values().collect());
        };
        if !seen_pages.insert(source.clone()) {
            return Err(AppError::InvalidSource(
                "Python 官方运行时索引出现循环".to_string(),
            ));
        }
        let index = fetch_index(source.clone()).await?;
        for runtime in index.versions {
            let Some(remote) = python_runtime_version(runtime) else {
                continue;
            };
            if let Some(previous) = versions.get(&remote.version) {
                if previous.download_url != remote.download_url
                    || previous.checksum != remote.checksum
                {
                    return Err(AppError::InvalidSource(
                        "Python 官方索引为同一版本提供了冲突的发行包".to_string(),
                    ));
                }
            } else {
                versions.insert(remote.version.clone(), remote);
            }
        }
        next = index
            .next
            .map(|next| python_next_index(&source, &next))
            .transpose()?;
    }
    if next.is_some() {
        return Err(AppError::InvalidSource(
            "Python 官方运行时索引页数超出限制".to_string(),
        ));
    }
    Ok(versions.into_values().collect())
}

fn python_next_index(source: &str, next: &str) -> AppResult<String> {
    let invalid =
        || AppError::InvalidSource("Python 官方运行时索引包含不受信任的下一页地址".to_string());
    let url = url::Url::parse(source)
        .map_err(|_| invalid())?
        .join(next)
        .map_err(|_| invalid())?;
    let suffix = url
        .path()
        .strip_prefix("/ftp/python/index-windows")
        .and_then(|path| path.strip_suffix(".json"));
    if url.scheme() != "https"
        || url.host_str() != Some("www.python.org")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || suffix.is_none_or(|suffix| {
            !suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(invalid());
    }
    Ok(url.to_string())
}

fn python_runtime_version(runtime: PythonRuntime) -> Option<RemoteVersion> {
    let minor = python_stable_minor(&runtime.version)?;
    if runtime.schema != 1
        || runtime.company != "PythonCore"
        || runtime.tag != format!("3.{minor}-64")
        || runtime.id != format!("pythoncore-3.{minor}-64")
        || !python_full_runtime_url(&runtime.version, &runtime.url)
    {
        return None;
    }
    // Legacy NuGet entries have no SHA-256 and lack parts of the full runtime.
    // Keep only official full ZIP packages; never fall back to an embedded build.
    let checksum = runtime
        .hash
        .as_ref()?
        .get("sha256")
        .filter(|hash| valid_sha256(hash))?
        .to_ascii_lowercase();
    Some(RemoteVersion {
        install_unavailable_reason: None,
        notes_url: Some(format!(
            "https://www.python.org/downloads/release/python-{}/",
            runtime.version.replace('.', "")
        )),
        version: runtime.version,
        channel: "stable".to_string(),
        published_at: None,
        architecture: "x86_64".to_string(),
        download_url: Some(runtime.url),
        checksum_algorithm: Some("sha256".to_string()),
        checksum: Some(checksum),
    })
}

fn python_full_runtime_url(version: &str, download_url: &str) -> bool {
    download_url
        == format!("https://www.python.org/ftp/python/{version}/python-{version}-amd64.zip")
}

fn python_stable_minor(version: &str) -> Option<&str> {
    let mut parts = version.split('.');
    let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
    (major == "3"
        && parts.next().is_none()
        && [minor, patch]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())))
    .then_some(minor)
}

pub(crate) fn is_supported_python_runtime(remote: &RemoteVersion) -> bool {
    if remote.install_unavailable_reason.is_some()
        || normalize_python_history_version(&remote.version).as_deref()
            != Some(remote.version.as_str())
        || !(remote.version.starts_with("2.") || remote.version.starts_with("3."))
    {
        return false;
    }
    let Some(download_url) = remote.download_url.as_deref() else {
        return false;
    };
    let valid_sha = remote
        .checksum_algorithm
        .as_deref()
        .is_some_and(|algorithm| algorithm.eq_ignore_ascii_case("sha256"))
        && remote.checksum.as_deref().is_some_and(valid_sha256);
    if python_stable_minor(&remote.version).is_some()
        && remote.architecture == "x86_64"
        && python_full_runtime_url(&remote.version, download_url)
    {
        return valid_sha;
    }
    (valid_sha
        || (remote
            .checksum_algorithm
            .as_deref()
            .is_some_and(|algorithm| algorithm.eq_ignore_ascii_case("md5"))
            && remote
                .checksum
                .as_deref()
                .is_some_and(|checksum| valid_hex_digest(checksum, 32))))
        && python_download_url(download_url).as_deref() == Some(download_url)
        && python_installer_architecture(&remote.version, download_url)
            == Some(remote.architecture.as_str())
        && python_supported_installer_format(&remote.version, download_url)
}

pub(crate) fn is_complete_python_catalog(catalog: &VersionCatalog) -> bool {
    if catalog.tool_id != "python"
        || catalog.source_name != PYTHON_HISTORY_NAME
        || catalog.source_url != PYTHON_RELEASE_SOURCE
        || catalog.versions.is_empty()
    {
        return false;
    }
    let mut seen = HashSet::new();
    catalog.versions.iter().all(|remote| {
        if !seen.insert(&remote.version)
            || normalize_python_history_version(&remote.version).as_deref()
                != Some(remote.version.as_str())
        {
            return false;
        }
        if is_supported_python_runtime(remote) {
            return true;
        }
        remote.download_url.is_none()
            && remote.checksum_algorithm.is_none()
            && remote.checksum.is_none()
            && remote.architecture == "source"
            && remote
                .install_unavailable_reason
                .as_deref()
                .is_some_and(|reason| !reason.trim().is_empty())
            && remote.notes_url.as_deref().is_some_and(|notes| {
                notes == PYTHON_LEGACY_SOURCE
                    || notes.starts_with(PYTHON_EARLY_SOURCE)
                    || notes.starts_with("https://www.python.org/downloads/release/")
                    || notes.starts_with(PYTHON_FTP_SOURCE)
                    || python_source_tag_notes(&remote.version, notes)
            })
    })
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
                    install_unavailable_reason: None,
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
                install_unavailable_reason: None,
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
        install_unavailable_reason: None,
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
    Ok(catalog(
        tool_id,
        "Node.js Releases",
        SOURCE,
        build_node_versions(releases),
    ))
}

fn build_node_versions(releases: Vec<NodeRelease>) -> Vec<RemoteVersion> {
    let mut versions = releases
        .into_iter()
        .filter(|release| release.files.iter().any(|file| file == "win-x64-zip"))
        .map(|release| {
            let filename = format!("node-{}-win-x64.zip", release.version);
            RemoteVersion {
                install_unavailable_reason: None,
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
    if tool_id == "python" {
        if let Some(reason) = &remote.install_unavailable_reason {
            return Err(AppError::InvalidSource(format!(
                "Python {} 暂不能安装：{reason}",
                remote.version
            )));
        }
        return if is_supported_python_runtime(remote) {
            Ok(())
        } else {
            Err(AppError::InvalidSource(
                "Python 发行包不是已校验的官方完整运行时或安装包，请刷新版本列表；嵌入式包不支持完整开发环境".to_string(),
            ))
        };
    }
    if tool_id != "node" {
        return Ok(());
    }
    let version = remote.version.trim_start_matches('v');
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err(AppError::InvalidSource(
            "Node.js 版本号格式无效".to_string(),
        ));
    }
    let expected_url = format!("https://nodejs.org/dist/v{version}/node-v{version}-win-x64.zip");
    if remote.download_url.as_deref() != Some(expected_url.as_str()) {
        return Err(AppError::InvalidSource(
            "Node.js 下载地址与所选版本的官方发行包不匹配".to_string(),
        ));
    }
    if remote
        .checksum_algorithm
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("sha256"))
        && remote.checksum.as_deref().is_some_and(valid_sha256)
    {
        return Ok(());
    }
    let checksum_url = format!("https://nodejs.org/dist/v{version}/SHASUMS256.txt");
    fill_node_checksum(client, remote, &checksum_url).await
}

async fn fill_node_checksum(
    client: &reqwest::Client,
    remote: &mut RemoteVersion,
    checksum_url: &str,
) -> AppResult<()> {
    let filename = format!(
        "node-v{}-win-x64.zip",
        remote.version.trim_start_matches('v')
    );
    let checksums = checked_text(client, checksum_url).await?;
    let matching = checksums
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (name == filename && parts.next().is_none()).then_some(hash)
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 || !valid_sha256(matching[0]) {
        return Err(AppError::InvalidSource(format!(
            "Node.js 官方校验清单缺少唯一有效的 {filename} SHA-256，已停止安装"
        )));
    }
    remote.checksum_algorithm = Some("sha256".to_string());
    remote.checksum = Some(matching[0].to_ascii_lowercase());
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    valid_hex_digest(value, 64)
}

fn valid_hex_digest(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
            install_unavailable_reason: None,
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
                    install_unavailable_reason: None,
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
                            install_unavailable_reason: None,
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
    let fetched = checked_github_releases(client, SOURCE).await?;
    let versions = fetched
        .releases
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
                install_unavailable_reason: None,
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
    Ok(github_catalog(
        tool_id,
        "RubyInstaller for Windows",
        fetched.web_source.as_deref().unwrap_or(SOURCE),
        fetched.partial,
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
                install_unavailable_reason: None,
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

struct GithubReleases {
    releases: Vec<GithubRelease>,
    partial: bool,
    web_source: Option<String>,
}

async fn fetch_github_release(
    client: &reqwest::Client,
    tool_id: &str,
    source_name: &str,
    source_url: &str,
    selector: AssetSelector,
) -> AppResult<VersionCatalog> {
    let fetched = checked_github_releases(client, source_url).await?;
    let versions = fetched
        .releases
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
                install_unavailable_reason: None,
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
    Ok(github_catalog(
        tool_id,
        source_name,
        fetched.web_source.as_deref().unwrap_or(source_url),
        fetched.partial,
        versions,
    ))
}

async fn checked_github_releases(
    client: &reqwest::Client,
    source_url: &str,
) -> AppResult<GithubReleases> {
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
                    return Ok(GithubReleases {
                        releases,
                        partial: true,
                        web_source: None,
                    });
                }
                let Some(repository) = official_github_repository(source_url) else {
                    return Err(error);
                };
                return match fetch_github_web_latest(client, repository).await {
                    Ok(release) => Ok(GithubReleases {
                        releases: vec![release],
                        partial: true,
                        web_source: Some(format!(
                            "https://github.com/{repository}/releases/latest"
                        )),
                    }),
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
    Ok(GithubReleases {
        releases,
        partial: false,
        web_source: None,
    })
}

fn github_api_access_limited(error: &AppError) -> bool {
    matches!(
        error,
        AppError::SourceHttp {
            status: 403 | 429,
            ..
        }
    )
}

fn official_github_repository(source_url: &str) -> Option<&'static str> {
    match source_url {
        "https://api.github.com/repos/git-for-windows/git/releases" => Some("git-for-windows/git"),
        "https://api.github.com/repos/Kitware/CMake/releases" => Some("Kitware/CMake"),
        "https://api.github.com/repos/oneclick/rubyinstaller2/releases" => {
            Some("oneclick/rubyinstaller2")
        }
        _ => None,
    }
}

async fn fetch_github_web_latest(
    client: &reqwest::Client,
    repository: &str,
) -> AppResult<GithubRelease> {
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
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
    {
        return Err(AppError::InvalidSource(
            "GitHub 最新发布页跳转到了不匹配的官方仓库或版本".to_string(),
        ));
    }
    Ok(tag.to_string())
}

fn parse_github_web_release(
    repository: &str,
    tag: &str,
    release_url: &str,
    html: &str,
) -> AppResult<GithubRelease> {
    let rows = Regex::new(r"(?is)<li\b[^>]*>(.*?)</li>")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let links = Regex::new(r#"(?i)\bhref\s*=\s*["']([^"']+)["']"#)
        .map_err(|error| AppError::Message(error.to_string()))?;
    let digests = Regex::new(r"(?i)\bsha256:([0-9a-f]{64})\b")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let base = url::Url::parse("https://github.com/")
        .map_err(|error| AppError::Message(error.to_string()))?;
    let prefix = format!("/{repository}/releases/download/{tag}/");
    let mut assets: Vec<GithubAsset> = Vec::new();
    let mut asset_indices: HashMap<String, usize> = HashMap::new();
    for row in rows.captures_iter(html) {
        let body = row
            .get(1)
            .map(|capture| capture.as_str())
            .unwrap_or_default();
        for link in links.captures_iter(body) {
            let Some(href) = link.get(1) else {
                continue;
            };
            let Ok(download_url) = base.join(href.as_str()) else {
                continue;
            };
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
            let Some(name) = download_url.path().strip_prefix(&prefix) else {
                continue;
            };
            if name.is_empty() || name.contains('/') || name.contains('%') {
                continue;
            }
            let hashes = digests
                .captures_iter(body)
                .filter_map(|capture| {
                    capture
                        .get(1)
                        .map(|value| value.as_str().to_ascii_lowercase())
                })
                .collect::<HashSet<_>>();
            if hashes.len() > 1
                || (body.to_ascii_lowercase().contains("sha256:") && hashes.is_empty())
            {
                return Err(AppError::InvalidSource(format!(
                    "GitHub 官方发布页中 {name} 的 SHA-256 不明确或无效"
                )));
            }
            let digest = hashes
                .into_iter()
                .next()
                .map(|hash| format!("sha256:{hash}"));
            if let Some(index) = asset_indices.get(download_url.as_str()) {
                let existing = &mut assets[*index];
                match (&existing.digest, &digest) {
                    (Some(previous), Some(current)) if previous != current => {
                        return Err(AppError::InvalidSource(format!(
                            "GitHub 官方发布页中 {name} 的重复下载链接提供了冲突的 SHA-256"
                        )));
                    }
                    (None, Some(_)) => existing.digest = digest,
                    _ => {}
                }
                continue;
            }
            asset_indices.insert(download_url.to_string(), assets.len());
            assets.push(GithubAsset {
                name: name.to_string(),
                browser_download_url: download_url.to_string(),
                digest,
            });
        }
    }
    if assets.is_empty() {
        return Err(AppError::InvalidSource(
            "GitHub 官方发布页没有返回该仓库的可下载发行包".to_string(),
        ));
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
            install_unavailable_reason: None,
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
        failures: Arc<Mutex<Vec<String>>>,
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
            let failures = Arc::new(Mutex::new(Vec::new()));
            let server_failures = failures.clone();
            let (stop, stopped) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut index = 0;
                while matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request = (|| -> std::io::Result<String> {
                                // Winsock accept inherits the listener's nonblocking mode.
                                // Reset it before waiting for a client's request bytes.
                                stream.set_nonblocking(false)?;
                                stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
                                stream
                                    .set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
                                let mut request = Vec::new();
                                let mut buffer = [0_u8; 2048];
                                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                                    match stream.read(&mut buffer) {
                                        Ok(0) => {
                                            return Err(std::io::Error::new(
                                                std::io::ErrorKind::UnexpectedEof,
                                                "mock client closed before sending its request headers",
                                            ));
                                        }
                                        Ok(length) => request.extend_from_slice(&buffer[..length]),
                                        Err(error)
                                            if error.kind() == std::io::ErrorKind::Interrupted =>
                                        {
                                            continue;
                                        }
                                        Err(error) => return Err(error),
                                    }
                                    if request.len() > 64 * 1024 {
                                        return Err(std::io::Error::new(
                                            std::io::ErrorKind::InvalidData,
                                            "mock request headers exceed the limit",
                                        ));
                                    }
                                }
                                let header_end = request
                                    .windows(4)
                                    .position(|window| window == b"\r\n\r\n")
                                    .unwrap()
                                    + 4;
                                let headers = std::str::from_utf8(&request[..header_end]).map_err(
                                    |error| {
                                        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                                    },
                                )?;
                                let content_length = headers
                                    .lines()
                                    .find_map(|line| {
                                        let (name, value) = line.split_once(':')?;
                                        name.eq_ignore_ascii_case("content-length")
                                            .then_some(value.trim())
                                    })
                                    .map(str::parse::<usize>)
                                    .transpose()
                                    .map_err(|error| {
                                        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                                    })?
                                    .unwrap_or(0);
                                if content_length > 64 * 1024 - header_end {
                                    return Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        "mock request body exceeds the limit",
                                    ));
                                }
                                while request.len() < header_end + content_length {
                                    match stream.read(&mut buffer) {
                                        Ok(0) => {
                                            return Err(std::io::Error::new(
                                                std::io::ErrorKind::UnexpectedEof,
                                                "mock client closed before sending its request body",
                                            ));
                                        }
                                        Ok(length) => request.extend_from_slice(&buffer[..length]),
                                        Err(error)
                                            if error.kind() == std::io::ErrorKind::Interrupted =>
                                        {
                                            continue;
                                        }
                                        Err(error) => return Err(error),
                                    }
                                }
                                String::from_utf8(request).map_err(|error| {
                                    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                                })
                            })();
                            let request = match request {
                                Ok(request) => request,
                                Err(error) => {
                                    server_failures.lock().unwrap().push(error.to_string());
                                    continue;
                                }
                            };
                            server_requests.lock().unwrap().push(request);
                            let response = &responses[index.min(responses.len() - 1)];
                            // Status-error tests may close the response without reading its body.
                            let _ = stream.write_all(response.as_bytes());
                            index += 1;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => {
                            server_failures
                                .lock()
                                .unwrap()
                                .push(format!("mock source accept: {error}"));
                            break;
                        }
                    }
                }
            });
            Self {
                url,
                requests,
                failures,
                stop,
                worker: Some(worker),
            }
        }

        fn request_count(&self) -> usize {
            let failures = self.failures.lock().unwrap();
            assert!(
                failures.is_empty(),
                "mock source failed: {}",
                failures.join("; ")
            );
            self.requests.lock().unwrap().len()
        }
    }

    impl Drop for MockSource {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(worker) = self.worker.take() {
                // A destructor must not panic while the test is already unwinding:
                // that aborts the entire harness and hides all earlier failures.
                if worker.join().is_err() {
                    let _ = writeln!(
                        std::io::stderr(),
                        "mock source worker panicked during cleanup"
                    );
                }
            }
        }
    }

    fn http_response(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn local_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap()
    }

    #[derive(Debug, Deserialize)]
    struct RetryFixture {
        value: String,
    }

    #[test]
    fn mock_source_waits_for_delayed_and_fragmented_request_headers() {
        let source = MockSource::new(vec![http_response("200 OK", "", "ready")]);
        let address = source.url.strip_prefix("http://").unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        stream.write_all(b"GET / HTTP/1.1\r\nHost: local").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        stream
            .write_all(b"host\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.ends_with("ready"));
        assert_eq!(source.request_count(), 1);
        assert!(source.requests.lock().unwrap()[0].contains("Host: localhost\r\n"));
    }

    #[test]
    fn github_rate_limit_stops_after_one_request_and_reports_reset_time() {
        let source = MockSource::new(vec![http_response(
            "403 Forbidden",
            "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 1792022400\r\n",
            r#"{"message":"API rate limit exceeded"}"#,
        )]);
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(checked_json::<RetryFixture>(&local_client(), &source.url))
            .unwrap_err();
        assert_eq!(source.request_count(), 1);
        assert!(matches!(
            error,
            AppError::SourceHttp {
                status: 403,
                rate_limited: true,
                ..
            }
        ));
        let message = error.to_string();
        assert!(message.contains("请求限额已用尽"));
        assert!(message.contains("UTC"));
        assert!(!message.contains("无法识别的数据"));
    }

    #[test]
    fn text_rate_limit_and_missing_json_are_not_retried() {
        let source = MockSource::new(vec![http_response(
            "429 Too Many Requests",
            "Retry-After: 120\r\n",
            "slow down",
        )]);
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(checked_text(&local_client(), &source.url))
            .unwrap_err();
        assert!(matches!(
            error,
            AppError::SourceHttp {
                status: 429,
                rate_limited: true,
                retry_at: Some(_),
                ..
            }
        ));
        assert_eq!(source.request_count(), 1);
        let missing = MockSource::new(vec![http_response("404 Not Found", "", "missing")]);
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(checked_json::<RetryFixture>(&local_client(), &missing.url))
            .unwrap_err();
        assert!(matches!(error, AppError::SourceHttp { status: 404, .. }));
        assert_eq!(missing.request_count(), 1);
    }

    #[test]
    fn source_retry_time_accepts_http_date_and_ignores_overflow() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 14 Oct 2026 16:00:00 GMT".parse().unwrap(),
        );
        assert_eq!(
            source_retry_time(&headers, Utc::now())
                .unwrap()
                .to_rfc3339(),
            "2026-10-14T16:00:00+00:00"
        );
        headers.insert(
            reqwest::header::RETRY_AFTER,
            i64::MAX.to_string().parse().unwrap(),
        );
        assert!(source_retry_time(&headers, Utc::now()).is_none());
    }

    #[test]
    fn temporary_source_failure_retries_and_schema_errors_do_not() {
        let source = MockSource::new(vec![
            http_response("503 Service Unavailable", "", "temporary"),
            http_response(
                "200 OK",
                "Content-Type: application/json\r\n",
                r#"{"value":"ready"}"#,
            ),
        ]);
        let parsed = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(checked_json::<RetryFixture>(&local_client(), &source.url))
            .unwrap();
        assert_eq!(parsed.value, "ready");
        assert_eq!(source.request_count(), 2);
        let malformed = MockSource::new(vec![http_response(
            "200 OK",
            "Content-Type: application/json\r\n",
            r#"{"wrong":true}"#,
        )]);
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(checked_json::<RetryFixture>(
                &local_client(),
                &malformed.url,
            ))
            .unwrap_err();
        assert!(matches!(error, AppError::InvalidSource(_)));
        assert_eq!(malformed.request_count(), 1);
    }

    #[test]
    fn server_retry_after_does_not_trigger_an_immediate_retry() {
        let source = MockSource::new(vec![http_response(
            "503 Service Unavailable",
            "Retry-After: 120\r\n",
            "temporary",
        )]);
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(checked_text(&local_client(), &source.url))
            .unwrap_err();
        assert!(matches!(
            error,
            AppError::SourceHttp {
                status: 503,
                retry_at: Some(_),
                ..
            }
        ));
        assert_eq!(source.request_count(), 1);
    }

    #[test]
    fn github_history_remains_available_when_a_later_page_hits_the_limit() {
        let release = serde_json::json!({
            "tag_name": "v2.50.1", "html_url": "https://github.com/git-for-windows/git/releases/tag/v2.50.1",
            "published_at": null, "prerelease": false, "draft": false, "assets": []
        });
        let page = serde_json::to_string(&vec![release; 100]).unwrap();
        let source = MockSource::new(vec![
            http_response("200 OK", "Content-Type: application/json\r\n", &page),
            http_response("403 Forbidden", "X-RateLimit-Remaining: 0\r\n", "limited"),
        ]);
        let releases = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(checked_github_releases(&local_client(), &source.url))
            .unwrap();
        assert_eq!(releases.releases.len(), 100);
        assert!(releases.partial);
        let partial = github_catalog(
            "git",
            "Git for Windows",
            &source.url,
            releases.partial,
            Vec::new(),
        );
        assert!(is_partial_catalog(&partial));
        let complete = github_catalog("git", "Git for Windows", &source.url, false, Vec::new());
        assert!(!is_partial_catalog(&complete));
        assert_eq!(source.request_count(), 2);
        let requests = source.requests.lock().unwrap();
        assert!(requests[0].contains("per_page=100&page=1"));
        assert!(requests[1].contains("per_page=100&page=2"));
    }

    fn node_release(version: &str) -> NodeRelease {
        NodeRelease {
            version: version.to_string(),
            date: "2026-10-14".to_string(),
            files: vec!["win-x64-zip".to_string()],
            lts: serde_json::Value::Bool(false),
        }
    }

    #[test]
    fn node_history_is_complete_and_selected_checksum_is_fetched_once() {
        let versions = build_node_versions(
            (0..80)
                .map(|index| node_release(&format!("v20.0.{index}")))
                .collect(),
        );
        assert_eq!(versions.len(), 80);
        assert!(versions.iter().all(|version| version.checksum.is_none()));
        let mut selected = versions[75].clone();
        let filename = format!("node-v{}-win-x64.zip", selected.version);
        let checksum = "a".repeat(64);
        let source = MockSource::new(vec![http_response(
            "200 OK",
            "",
            &format!(
                "{}  unrelated.zip\n{checksum}  {filename}\n",
                "b".repeat(64)
            ),
        )]);
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fill_node_checksum(
                &local_client(),
                &mut selected,
                &source.url,
            ))
            .unwrap();
        assert_eq!(source.request_count(), 1);
        assert_eq!(selected.checksum.as_deref(), Some(checksum.as_str()));
        assert_eq!(selected.checksum_algorithm.as_deref(), Some("sha256"));
    }

    #[test]
    fn selected_node_metadata_rejects_missing_invalid_or_ambiguous_checksums() {
        for body in [
            "abcd  node-v20.0.1-win-x64.zip\n".to_string(),
            format!(
                "{}  node-v20.0.1-win-x64.zip\n{}  node-v20.0.1-win-x64.zip\n",
                "a".repeat(64),
                "b".repeat(64)
            ),
            format!("{}  node-v20.0.2-win-x64.zip\n", "a".repeat(64)),
        ] {
            let mut selected = build_node_versions(vec![node_release("v20.0.1")]).remove(0);
            let source = MockSource::new(vec![http_response("200 OK", "", &body)]);
            assert!(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(fill_node_checksum(
                        &local_client(),
                        &mut selected,
                        &source.url
                    ))
                    .is_err()
            );
            assert_eq!(source.request_count(), 1);
            assert!(selected.checksum.is_none());
        }
    }

    #[test]
    fn selected_node_metadata_reuses_valid_checksum_and_rejects_untrusted_url() {
        let mut selected = build_node_versions(vec![node_release("v20.0.1")]).remove(0);
        selected.checksum_algorithm = Some("sha256".to_string());
        selected.checksum = Some("a".repeat(64));
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(ensure_install_metadata(
                &local_client(),
                "node",
                &mut selected,
            ))
            .unwrap();
        selected.download_url = Some("https://nodejs.org.evil.invalid/dist/node.zip".to_string());
        assert!(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(ensure_install_metadata(
                    &local_client(),
                    "node",
                    &mut selected
                ))
                .is_err()
        );
    }

    #[test]
    fn github_web_fallback_keeps_official_asset_digests_and_rejects_foreign_links() {
        let checksum = "a".repeat(64);
        for (repository, tag, filename, selector) in [
            (
                "git-for-windows/git",
                "v2.50.1.windows.1",
                "PortableGit-2.50.1-64-bit.7z.exe",
                select_portable_git as AssetSelector,
            ),
            (
                "Kitware/CMake",
                "v4.0.1",
                "cmake-4.0.1-windows-x86_64.zip",
                select_cmake_zip as AssetSelector,
            ),
            (
                "oneclick/rubyinstaller2",
                "RubyInstaller-3.4.1-1",
                "rubyinstaller-3.4.1-1-x64.7z",
                select_ruby_archive as AssetSelector,
            ),
        ] {
            let fixture = format!(
                r#"<ul><li><a href="/{repository}/releases/download/{tag}/{filename}">download</a><clipboard-copy value="sha256:{checksum}"></clipboard-copy></li><li><a href="https://evil.invalid/{filename}">fake</a></li><li><a href="/someone/else/releases/download/{tag}/{filename}">fake</a></li></ul>"#
            );
            let page = format!("https://github.com/{repository}/releases/tag/{tag}");
            let release = parse_github_web_release(repository, tag, &page, &fixture).unwrap();
            assert_eq!(release.assets.len(), 1);
            let selected = selector(&release.assets).unwrap();
            assert_eq!(selected.name, filename);
            assert_eq!(
                selected.digest.as_deref(),
                Some(format!("sha256:{checksum}").as_str())
            );
            assert_eq!(
                official_github_release_tag(repository, &url::Url::parse(&page).unwrap()).unwrap(),
                tag
            );
            assert!(
                official_github_release_tag(
                    repository,
                    &url::Url::parse(&page.replace("github.com", "github.com.evil.invalid"))
                        .unwrap()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn github_web_fallback_rejects_conflicting_digests() {
        let fixture = format!(
            r#"<li><a href="/git-for-windows/git/releases/download/v2.50.1/PortableGit-2.50.1-64-bit.7z.exe">download</a>sha256:{} sha256:{}</li>"#,
            "a".repeat(64),
            "b".repeat(64)
        );
        assert!(
            parse_github_web_release(
                "git-for-windows/git",
                "v2.50.1",
                "https://github.com/git-for-windows/git/releases/tag/v2.50.1",
                &fixture
            )
            .is_err()
        );
    }

    #[test]
    fn github_web_duplicate_asset_merges_a_later_digest() {
        let path =
            "/git-for-windows/git/releases/download/v2.50.1/PortableGit-2.50.1-64-bit.7z.exe";
        let checksum = "a".repeat(64);
        let fixture = format!(
            r#"<li><a href="{path}">download</a></li><li><a href="https://github.com{path}">download</a><clipboard-copy value="sha256:{checksum}"></clipboard-copy></li>"#
        );
        let release = parse_github_web_release(
            "git-for-windows/git",
            "v2.50.1",
            "https://github.com/git-for-windows/git/releases/tag/v2.50.1",
            &fixture,
        )
        .unwrap();
        assert_eq!(release.assets.len(), 1);
        assert_eq!(
            release.assets[0].digest.as_deref(),
            Some(format!("sha256:{checksum}").as_str())
        );
    }

    #[test]
    fn github_web_duplicate_asset_rejects_conflicting_digests_across_rows() {
        let path =
            "/git-for-windows/git/releases/download/v2.50.1/PortableGit-2.50.1-64-bit.7z.exe";
        let fixture = format!(
            r#"<li><a href="{path}">download</a>sha256:{}</li><li><a href="{path}">download</a>sha256:{}</li>"#,
            "a".repeat(64),
            "b".repeat(64)
        );
        let error = parse_github_web_release(
            "git-for-windows/git",
            "v2.50.1",
            "https://github.com/git-for-windows/git/releases/tag/v2.50.1",
            &fixture,
        )
        .unwrap_err();
        assert!(error.to_string().contains("冲突的 SHA-256"));
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

    fn python_release_fixture(version: &str, preview: bool) -> PythonRelease {
        PythonRelease {
            name: format!("Python {version}"),
            slug: format!("python-{}", version.replace('.', "")),
            version: version.split('.').next().unwrap().parse().unwrap(),
            is_published: true,
            pre_release: preview,
            release_date: Some("2026-10-17T00:00:00Z".into()),
            resource_uri: format!("https://www.python.org/api/v2/downloads/release/{version}/"),
        }
    }

    fn python_installer_fixture(version: &str, architecture: &str) -> PythonReleaseFile {
        let suffix = if architecture == "x86_64" {
            ".amd64.msi"
        } else {
            ".msi"
        };
        PythonReleaseFile {
            name: format!(
                "Windows {} MSI installer",
                if architecture == "x86_64" {
                    "x86-64"
                } else {
                    "x86"
                }
            ),
            os: "https://www.python.org/api/v2/downloads/os/1/".into(),
            release: python_release_fixture(version, false).resource_uri,
            is_source: false,
            url: format!(
                "https://www.python.org/ftp/python/{}/python-{version}{suffix}",
                python_release_base(version)
            ),
            md5_sum: "a".repeat(32),
            sha256_sum: String::new(),
        }
    }

    const PYTHON_LEGACY_FIXTURE: &str = r#"
        <a class="reference external" href="1.6.1">Python 1.6.1</a>
        <a class="reference external" href="1.5">Python 1.5.2</a>
        <a href="python1.0.1.tar.gz">source</a>
        <a href="py152.tgz">source</a>
        <a href="python-1.5.2c1.tar.gz">source</a>
        <a href="lib1.2.tar.gz">not an interpreter</a>
    "#;
    const PYTHON_FTP_FIXTURE: &str = r#"
        <a href="2.1/">archived directory</a>
        <a href="3.16.0/">also contains prereleases</a>
        <a href="contrib/">not a version</a>
    "#;

    fn python_git_fixture(records: &[&str]) -> Vec<u8> {
        let mut bytes = b"001e# service=git-upload-pack\n0000".to_vec();
        bytes.extend_from_slice(&python_ls_refs_fixture(records));
        bytes
    }

    fn python_ls_refs_fixture(records: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for record in records {
            let packet = format!("{record}\n");
            bytes.extend_from_slice(format!("{:04x}", packet.len() + 4).as_bytes());
            bytes.extend_from_slice(packet.as_bytes());
        }
        bytes.extend_from_slice(b"0000");
        bytes
    }

    const PYTHON_REAL_V2_CAPABILITIES: &str = concat!(
        "001e# service=git-upload-pack\n0000000eversion 2\n",
        "0028agent=git/github-41cc7379a75f-Linux\n",
        "0013ls-refs=unborn\n0027fetch=shallow wait-for-done filter\n",
        "0012server-option\n0017object-format=sha1\n0000",
    );

    #[test]
    fn python_v2_queries_only_version_tags_with_a_checked_discovery_and_post() {
        let tags = String::from_utf8(python_ls_refs_fixture(&[
            "1f08c772f5a68bdf8309d6ad1cdddb838fc35feb refs/tags/v3.4.0a1",
            "bd61c3ae1f45c402d8e09fdebeeabbe68132bb52 refs/tags/v3.4.0a4",
            "7e611dab4a05a97e708541d98e2f6817cfbe7453 refs/tags/v3.4.0b1",
            "e9f73ac1fd5184173179596d4fa5caa45682d301 refs/tags/v3.4.0rc1",
            "7a325c385bb547b481abfee8301e9d9f2e58990a refs/tags/v1.0.2",
            "c94d53db0e54d919ed3d37b85bd8d6e7f5a64a9e refs/tags/v1.1",
            "c7afd2c7bbdbb0b9bb47f0eb088285aec8ec7414 refs/tags/v1.5.2c1",
        ]))
        .unwrap();
        let source = MockSource::new(vec![
            http_response(
                "200 OK",
                "Content-Type: application/x-git-upload-pack-advertisement\r\n",
                PYTHON_REAL_V2_CAPABILITIES,
            ),
            http_response(
                "200 OK",
                "Content-Type: application/x-git-upload-pack-result\r\n",
                &tags,
            ),
        ]);
        let versions = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fetch_python_source_tags_from(
                &local_client(),
                &format!(
                    "{}/python/cpython.git/info/refs?service=git-upload-pack",
                    source.url
                ),
                &format!("{}/python/cpython.git/git-upload-pack", source.url),
            ))
            .unwrap();
        assert_eq!(versions.len(), 7);
        assert!(versions.contains(&("3.4.0a1".into(), "3.4.0a1".into())));
        assert!(versions.contains(&("3.4.0a4".into(), "3.4.0a4".into())));
        assert!(versions.contains(&("3.4.0b1".into(), "3.4.0b1".into())));
        assert!(versions.contains(&("3.4.0rc1".into(), "3.4.0rc1".into())));
        assert!(versions.contains(&("1.1.0".into(), "1.1".into())));
        assert!(versions.contains(&("1.5.2rc1".into(), "1.5.2c1".into())));
        assert_eq!(source.request_count(), 2);
        let requests = source.requests.lock().unwrap();
        let has_header = |headers: &str, expected_name: &str, expected_value: &str| {
            headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .any(|(name, value)| {
                    name.eq_ignore_ascii_case(expected_name)
                        && value.trim().eq_ignore_ascii_case(expected_value)
                })
        };
        let (get, get_body) = requests[0].split_once("\r\n\r\n").unwrap();
        assert!(
            get.starts_with(
                "GET /python/cpython.git/info/refs?service=git-upload-pack HTTP/1.1\r\n"
            )
        );
        assert!(has_header(get, "Git-Protocol", "version=2"));
        assert!(has_header(
            get,
            "Accept",
            "application/x-git-upload-pack-advertisement"
        ));
        assert!(get_body.is_empty());
        let (post, body) = requests[1].split_once("\r\n\r\n").unwrap();
        assert!(post.starts_with("POST /python/cpython.git/git-upload-pack HTTP/1.1\r\n"));
        assert!(has_header(post, "Git-Protocol", "version=2"));
        assert!(has_header(
            post,
            "Content-Type",
            "application/x-git-upload-pack-request"
        ));
        assert!(has_header(
            post,
            "Accept",
            "application/x-git-upload-pack-result"
        ));
        assert_eq!(
            body,
            "0014command=ls-refs\n0001001bref-prefix refs/tags/v\n0000"
        );
    }

    #[test]
    fn python_v2_discovery_and_post_keep_http_errors_and_reject_wrong_response_types() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for (responses, status, expected_count) in [
            (
                vec![http_response(
                    "429 Too Many Requests",
                    "Retry-After: 120\r\n",
                    "slow down",
                )],
                429,
                1,
            ),
            (
                vec![
                    http_response(
                        "200 OK",
                        "Content-Type: application/x-git-upload-pack-advertisement\r\n",
                        PYTHON_REAL_V2_CAPABILITIES,
                    ),
                    http_response(
                        "403 Forbidden",
                        "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 1792022400\r\n",
                        "blocked",
                    ),
                ],
                403,
                2,
            ),
        ] {
            let source = MockSource::new(responses);
            let error = runtime
                .block_on(fetch_python_source_tags_from(
                    &local_client(),
                    &source.url,
                    &source.url,
                ))
                .unwrap_err();
            assert!(
                matches!(error, AppError::SourceHttp { status: actual, rate_limited: true, retry_at: Some(_), .. } if actual == status)
            );
            assert_eq!(source.request_count(), expected_count);
        }
        let valid_tags = String::from_utf8(python_ls_refs_fixture(&[
            "1f08c772f5a68bdf8309d6ad1cdddb838fc35feb refs/tags/v3.4.0a1",
        ]))
        .unwrap();
        for (responses, expected_count) in [
            (
                vec![http_response(
                    "200 OK",
                    "Content-Type: text/html\r\n",
                    PYTHON_REAL_V2_CAPABILITIES,
                )],
                1,
            ),
            (
                vec![http_response(
                    "200 OK",
                    "Content-Type: application/x-git-upload-pack-advertisement\r\n",
                    "000eversion 2\n0000",
                )],
                1,
            ),
            (
                vec![http_response(
                    "200 OK",
                    "Content-Type: application/x-git-upload-pack-advertisement\r\n",
                    &String::from_utf8(python_git_fixture(&[
                        "1f08c772f5a68bdf8309d6ad1cdddb838fc35feb refs/tags/v3.4.0a1",
                    ]))
                    .unwrap(),
                )],
                1,
            ),
            (
                vec![
                    http_response(
                        "200 OK",
                        "Content-Type: application/x-git-upload-pack-advertisement\r\n",
                        PYTHON_REAL_V2_CAPABILITIES,
                    ),
                    http_response("200 OK", "Content-Type: text/html\r\n", &valid_tags),
                ],
                2,
            ),
            (
                vec![
                    http_response(
                        "200 OK",
                        "Content-Type: application/x-git-upload-pack-advertisement\r\n",
                        PYTHON_REAL_V2_CAPABILITIES,
                    ),
                    http_response(
                        "200 OK",
                        "Content-Type: application/x-git-upload-pack-result\r\n",
                        PYTHON_REAL_V2_CAPABILITIES,
                    ),
                ],
                2,
            ),
        ] {
            let source = MockSource::new(responses);
            let error = runtime
                .block_on(fetch_python_source_tags_from(
                    &local_client(),
                    &source.url,
                    &source.url,
                ))
                .unwrap_err();
            assert!(matches!(error, AppError::InvalidSource(_)));
            assert_eq!(source.request_count(), expected_count);
        }
    }

    #[test]
    fn python_v2_limits_both_declared_and_chunked_response_bodies() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let declared = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            PYTHON_TAGS_MAX_BYTES + 1,
        );
        // No Content-Length: enforce the same bound while accumulating chunks.
        let chunk = "x".repeat(PYTHON_TAGS_MAX_BYTES + 1);
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{chunk}\r\n0\r\n\r\n",
            chunk.len(),
        );
        drop(chunk);
        for response in [declared, chunked] {
            let source = MockSource::new(vec![response]);
            let error = runtime
                .block_on(fetch_python_source_tags_from(
                    &local_client(),
                    &source.url,
                    &source.url,
                ))
                .unwrap_err();
            assert!(matches!(error, AppError::InvalidSource(_)));
            assert!(error.to_string().contains("32 MiB"));
            assert_eq!(source.request_count(), 1);
        }
    }

    #[test]
    fn python_v2_requires_complete_capabilities_and_version_tag_ref_records() {
        assert!(parse_python_git_capabilities(PYTHON_REAL_V2_CAPABILITIES.as_bytes()).is_ok());
        assert!(parse_python_git_capabilities(b"000eversion 2\n000cls-refs\n0000").is_ok());
        let record = "1f08c772f5a68bdf8309d6ad1cdddb838fc35feb refs/tags/v3.4.0a1";
        for capabilities in [
            b"000eversion 2\n0000".to_vec(),
            b"000eversion 1\n000cls-refs\n0000".to_vec(),
            b"000eversion 2\n0010not-ls-refs\n0000".to_vec(),
            PYTHON_REAL_V2_CAPABILITIES.as_bytes()[..PYTHON_REAL_V2_CAPABILITIES.len() - 4]
                .to_vec(),
            [PYTHON_REAL_V2_CAPABILITIES.as_bytes(), b"0000"].concat(),
            python_git_fixture(&[record]),
        ] {
            assert!(parse_python_git_capabilities(&capabilities).is_err());
        }
        let valid = python_ls_refs_fixture(&[record]);
        for invalid in [
            b"0000".to_vec(),
            b"0001".to_vec(),
            PYTHON_REAL_V2_CAPABILITIES.as_bytes().to_vec(),
            python_git_fixture(&[record]),
            valid[..valid.len() - 1].to_vec(),
            valid[..valid.len() - 4].to_vec(),
            [valid.as_slice(), b"junk"].concat(),
        ] {
            assert!(parse_python_ls_refs(&invalid).is_err());
        }
        for reference in [
            "HEAD",
            "refs/heads/main",
            "refs/pull/123/head",
            "refs/tags/legacy",
            "refs/tags/v3.4.0a1^{}",
            "refs/tags/v3.4.0a1 peeled:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "refs/tags/v3.4.0a1/evil",
            "refs/tags/v3.4.0a1\0capabilities",
        ] {
            let invalid = format!("{} {reference}", "a".repeat(40));
            assert!(
                parse_python_ls_refs(&python_ls_refs_fixture(&[record, &invalid])).is_err(),
                "{reference}"
            );
        }
        for object in [
            "a".repeat(39),
            "g".repeat(40),
            "a".repeat(41),
            "a".repeat(63),
            "a".repeat(65),
        ] {
            let invalid = format!("{object} refs/tags/v3.4.0a1");
            assert!(parse_python_ls_refs(&python_ls_refs_fixture(&[&invalid])).is_err());
        }
        let sha256 = format!("{} refs/tags/v3.4.0a1", "a".repeat(64));
        assert_eq!(
            parse_python_ls_refs(&python_ls_refs_fixture(&[&sha256]))
                .unwrap()
                .len(),
            1
        );
    }

    fn python_real_source_tags() -> Vec<(String, String)> {
        // Actual official v0 advertisement records, including labels missing
        // from the python.org release database and old c -> rc spellings.
        parse_python_source_tags(&python_git_fixture(&[
            "010bebbf9fd12bba1e3f40bdbddacd51a9c2bf20 HEAD\0multi_ack thin-pack object-format=sha1",
            "010bebbf9fd12bba1e3f40bdbddacd51a9c2bf20 refs/heads/main",
            "010bebbf9fd12bba1e3f40bdbddacd51a9c2bf20 refs/pull/123/head",
            "1f08c772f5a68bdf8309d6ad1cdddb838fc35feb refs/tags/v3.4.0a1",
            "bd61c3ae1f45c402d8e09fdebeeabbe68132bb52 refs/tags/v3.4.0a4",
            "7e611dab4a05a97e708541d98e2f6817cfbe7453 refs/tags/v3.4.0b1",
            "e9f73ac1fd5184173179596d4fa5caa45682d301 refs/tags/v3.4.0rc1",
            "7a325c385bb547b481abfee8301e9d9f2e58990a refs/tags/v1.0.2",
            "c94d53db0e54d919ed3d37b85bd8d6e7f5a64a9e refs/tags/v1.1",
            "c7afd2c7bbdbb0b9bb47f0eb088285aec8ec7414 refs/tags/v1.5.2c1",
            "c7afd2c7bbdbb0b9bb47f0eb088285aec8ec7414 refs/tags/v1.5.2c1^{}",
            "c7afd2c7bbdbb0b9bb47f0eb088285aec8ec7414 refs/tags/legacy-label",
        ]))
        .unwrap()
    }

    #[test]
    fn python_source_tags_add_missing_previews_and_old_labels_without_replacing_packages() {
        let mut rows = build_python_history(
            vec![python_release_fixture("3.4.0a1", true)],
            vec![python_installer_fixture("3.4.0a1", "x86_64")],
            Vec::new(),
            PYTHON_LEGACY_FIXTURE,
            PYTHON_FTP_FIXTURE,
        )
        .unwrap();
        let original = rows
            .iter()
            .find(|row| row.version == "3.4.0a1")
            .unwrap()
            .clone();
        let tags = python_real_source_tags();
        assert_eq!(tags.len(), 7);
        assert!(tags.contains(&("1.1.0".into(), "1.1".into())));
        assert!(tags.contains(&("1.5.2rc1".into(), "1.5.2c1".into())));
        merge_python_source_tags(&mut rows, tags);
        let package = rows.iter().find(|row| row.version == "3.4.0a1").unwrap();
        assert_eq!(package.download_url, original.download_url);
        assert_eq!(package.checksum, original.checksum);
        assert_eq!(package.notes_url, original.notes_url);
        assert!(is_supported_python_runtime(package));
        assert_eq!(
            rows.iter().filter(|row| row.version == "3.4.0a1").count(),
            1
        );
        for version in ["3.4.0a4", "3.4.0b1", "3.4.0rc1"] {
            let row = rows.iter().find(|row| row.version == version).unwrap();
            assert_eq!(row.channel, "preview");
            assert!(row.download_url.is_none());
            assert!(row.checksum.is_none());
            assert!(row.install_unavailable_reason.is_some());
            assert_eq!(
                row.notes_url,
                Some(format!("https://github.com/python/cpython/tree/v{version}"))
            );
        }
        let old = rows.iter().find(|row| row.version == "1.1.0").unwrap();
        assert_eq!(old.channel, "archive");
        assert_eq!(
            old.notes_url.as_deref(),
            Some("https://github.com/python/cpython/tree/v1.1")
        );
        assert!(is_complete_python_catalog(&catalog(
            "python",
            PYTHON_HISTORY_NAME,
            PYTHON_RELEASE_SOURCE,
            rows,
        )));

        let mut tags_only = Vec::new();
        merge_python_source_tags(&mut tags_only, python_real_source_tags());
        let alpha = tags_only
            .iter()
            .find(|row| row.version == "3.4.0a1")
            .unwrap();
        assert!(alpha.download_url.is_none());
        assert_eq!(alpha.channel, "preview");
        assert!(python_source_tag_notes(
            &alpha.version,
            alpha.notes_url.as_deref().unwrap()
        ));
    }

    #[test]
    fn python_source_tag_cache_notes_require_the_exact_official_repository_and_version() {
        assert!(python_source_tag_notes(
            "1.1.0",
            "https://github.com/python/cpython/tree/v1.1"
        ));
        assert!(python_source_tag_notes(
            "1.5.2rc1",
            "https://github.com/python/cpython/tree/v1.5.2c1"
        ));
        for notes in [
            "http://github.com/python/cpython/tree/v3.4.0a1",
            "https://github.com.evil.test/python/cpython/tree/v3.4.0a1",
            "https://attacker@github.com/python/cpython/tree/v3.4.0a1",
            "https://github.com:444/python/cpython/tree/v3.4.0a1",
            "https://github.com/another/cpython/tree/v3.4.0a1",
            "https://github.com/python/cpython-other/tree/v3.4.0a1",
            "https://github.com/python/cpython/tree/v3.4.0a1?raw=1",
            "https://github.com/python/cpython/tree/v3.4.0a1#fragment",
            "https://github.com/python/cpython/tree/v3.4.0a1/",
            "https://github.com/python/cpython/tree/v%33.4.0a1",
            "https://github.com/python/cpython/tree/v3.4.0a1/../v3.4.0a1",
            "https://github.com/python/cpython/tree/v3.4.0",
        ] {
            assert!(!python_source_tag_notes("3.4.0a1", notes), "{notes}");
            let mut row = python_archive_row(
                "3.4.0a1".into(),
                notes.into(),
                "No verified package metadata",
            );
            row.channel = "preview".into();
            assert!(
                !is_complete_python_catalog(&catalog(
                    "python",
                    PYTHON_HISTORY_NAME,
                    PYTHON_RELEASE_SOURCE,
                    vec![row],
                )),
                "{notes}"
            );
        }
    }

    #[test]
    fn python_git_advertisement_rejects_bad_packets_hashes_namespaces_and_partial_results() {
        let valid_record = "1f08c772f5a68bdf8309d6ad1cdddb838fc35feb refs/tags/v3.4.0a1";
        let valid = python_git_fixture(&[valid_record]);
        for malformed in [
            Vec::new(),
            b"0001".to_vec(),
            b"000eversion 2\n0000".to_vec(),
            b"zzzz# service=git-upload-pack\n0000".to_vec(),
            b"001e# service=git-upload-pack\n00000002".to_vec(),
            b"001e# service=git-upload-pack\n0000ffff".to_vec(),
            valid[..valid.len() - 1].to_vec(),
            valid[..valid.len() - 4].to_vec(),
            [valid.clone(), b"junk".to_vec()].concat(),
        ] {
            assert!(parse_python_source_tags(&malformed).is_err());
        }
        for object in [
            "a".repeat(39),
            "a".repeat(41),
            "g".repeat(40),
            "a".repeat(63),
            "a".repeat(65),
        ] {
            let record = format!("{object} refs/tags/v3.4.0a1");
            assert!(
                parse_python_source_tags(&python_git_fixture(&[valid_record, &record])).is_err()
            );
        }
        let sha256 = format!(
            "{} refs/tags/v3.4.0a1\0object-format=sha256",
            "a".repeat(64)
        );
        assert_eq!(
            parse_python_source_tags(&python_git_fixture(&[&sha256]))
                .unwrap()
                .len(),
            1
        );
        for reference in [
            "refs/remotes/fork/tags/v3.4.0a1",
            "refs/heads/../tags/v3.4.0a1",
            "https://evil.test/refs/tags/v3.4.0a1",
            "refs/pull/123/other",
            "refs/tags/v3.4.0a1\0late-capabilities",
            "refs/tags/v3.4.0a1 extra",
        ] {
            let record = format!("{} {reference}", "a".repeat(40));
            assert!(
                parse_python_source_tags(&python_git_fixture(&[valid_record, &record])).is_err(),
                "{reference}"
            );
        }
        let conflicting = format!("{} refs/tags/v3.4.0a1", "a".repeat(40));
        assert!(
            parse_python_source_tags(&python_git_fixture(&[valid_record, &conflicting])).is_err()
        );
        let no_versions = format!("{} refs/heads/main", "a".repeat(40));
        assert!(parse_python_source_tags(&python_git_fixture(&[&no_versions])).is_err());
    }

    #[test]
    fn duplicate_official_python_release_records_merge_without_losing_installers() {
        let version = "3.3.5rc1";
        let mut second = python_release_fixture(version, true);
        second.resource_uri.push_str("second/");
        let mut empty = python_release_fixture(version, true);
        empty.resource_uri.push_str("empty/");
        let x86 = python_installer_fixture(version, "x86");
        let mut x64 = python_installer_fixture(version, "x86_64");
        x64.release = second.resource_uri.clone();
        let rows = build_python_history(
            vec![python_release_fixture(version, true), second, empty],
            vec![x86, x64],
            Vec::new(),
            PYTHON_LEGACY_FIXTURE,
            PYTHON_FTP_FIXTURE,
        )
        .unwrap();
        let matching = rows
            .iter()
            .filter(|row| row.version == version)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].architecture, "x86_64");
        assert!(matching[0].install_unavailable_reason.is_none());
        assert!(
            matching[0]
                .download_url
                .as_deref()
                .unwrap()
                .ends_with(".amd64.msi")
        );

        let mut second = python_release_fixture(version, true);
        second.resource_uri.push_str("second/");
        let first = python_installer_fixture(version, "x86_64");
        let mut conflicting = python_installer_fixture(version, "x86_64");
        conflicting.release = second.resource_uri.clone();
        conflicting.md5_sum = "b".repeat(32);
        assert!(
            build_python_history(
                vec![python_release_fixture(version, true), second],
                vec![first, conflicting],
                Vec::new(),
                PYTHON_LEGACY_FIXTURE,
                PYTHON_FTP_FIXTURE,
            )
            .is_err()
        );
    }

    #[test]
    fn python_history_merges_bulk_releases_previews_archives_and_prefers_full_zip() {
        let releases = vec![
            python_release_fixture("2.7.18", false),
            python_release_fixture("3.10.11", false),
            python_release_fixture("3.10.0rc1", true),
            python_release_fixture("3.10.22", false),
            python_release_fixture("3.15.0", false),
            python_release_fixture("3.16.0a1", true),
        ];
        let mut exe = python_installer_fixture("3.10.11", "x86_64");
        exe.name = "Windows installer (64-bit)".into();
        exe.url = "https://www.python.org/ftp/python/3.10.11/python-3.10.11-amd64.exe".into();
        let mut zip_replacement = python_installer_fixture("3.15.0", "x86_64");
        zip_replacement.md5_sum = "b".repeat(32);
        let versions = build_python_history(
            releases,
            vec![
                python_installer_fixture("2.7.18", "x86_64"),
                exe,
                python_installer_fixture("3.10.0rc1", "x86_64"),
                zip_replacement,
            ],
            vec![python_runtime_version(python_runtime_fixture("3.15.0")).unwrap()],
            PYTHON_LEGACY_FIXTURE,
            PYTHON_FTP_FIXTURE,
        )
        .unwrap();
        let versions = versions
            .into_iter()
            .map(|remote| (remote.version.clone(), remote))
            .collect::<HashMap<_, _>>();
        assert_eq!(versions.len(), 11);
        assert_eq!(
            versions["2.7.18"].checksum_algorithm.as_deref(),
            Some("md5")
        );
        assert_eq!(
            versions["3.10.11"].checksum_algorithm.as_deref(),
            Some("md5")
        );
        assert_eq!(versions["3.10.0rc1"].channel, "preview");
        assert!(is_supported_python_runtime(&versions["3.10.0rc1"]));
        assert_eq!(
            versions["3.15.0"].checksum_algorithm.as_deref(),
            Some("sha256")
        );
        assert!(
            versions["3.15.0"]
                .download_url
                .as_ref()
                .unwrap()
                .ends_with("-amd64.zip")
        );
        assert_eq!(
            versions["3.15.0"].published_at.as_deref(),
            Some("2026-10-17T00:00:00Z")
        );
        assert!(versions["3.10.22"].download_url.is_none());
        assert!(versions["3.10.22"].install_unavailable_reason.is_some());
        assert_eq!(versions["1.5.2rc1"].channel, "preview");
        assert!(versions["1.0.1"].download_url.is_none());
        assert_eq!(
            versions["1.6.1"].notes_url.as_deref(),
            Some("https://www.python.org/download/releases/1.6.1")
        );
        assert_eq!(
            versions["1.5.2"].notes_url.as_deref(),
            Some("https://www.python.org/download/releases/1.5")
        );
        assert_eq!(versions["2.1.0"].channel, "archive");
        assert!(!versions.contains_key("3.16.0"));
        let history = catalog(
            "python",
            PYTHON_HISTORY_NAME,
            PYTHON_RELEASE_SOURCE,
            versions.into_values().collect(),
        );
        assert!(is_complete_python_catalog(&history));
    }

    #[test]
    fn python_history_prefers_x64_then_checksum_and_supports_x86_when_needed() {
        let mut x86 = python_installer_fixture("2.7.18", "x86");
        x86.sha256_sum = "b".repeat(64);
        let mut x86_only = python_installer_fixture("2.4.0", "x86");
        x86_only.url = "https://www.python.org/ftp/python/2.4/python-2.4.msi".into();
        let versions = build_python_history(
            vec![
                python_release_fixture("2.7.18", false),
                python_release_fixture("2.4.0", false),
            ],
            vec![x86, python_installer_fixture("2.7.18", "x86_64"), x86_only],
            vec![],
            PYTHON_LEGACY_FIXTURE,
            PYTHON_FTP_FIXTURE,
        )
        .unwrap();
        let x64 = versions
            .iter()
            .find(|remote| remote.version == "2.7.18")
            .unwrap();
        assert_eq!(x64.architecture, "x86_64");
        assert_eq!(x64.checksum_algorithm.as_deref(), Some("md5"));
        let x86 = versions
            .iter()
            .find(|remote| remote.version == "2.4.0")
            .unwrap();
        assert_eq!(x86.architecture, "x86");
        assert!(is_supported_python_runtime(x86));
    }

    #[test]
    fn python_installer_metadata_rejects_embedding_web_wrong_versions_and_untrusted_urls() {
        let version = "3.10.11";
        for (name, url) in [
            (
                "Windows embeddable package (64-bit)",
                "https://www.python.org/ftp/python/3.10.11/python-3.10.11-amd64.exe",
            ),
            (
                "Windows x86-64 web-based installer",
                "https://www.python.org/ftp/python/3.10.11/python-3.10.11-amd64.exe",
            ),
            (
                "Windows debug installer",
                "https://www.python.org/ftp/python/3.10.11/python-3.10.11-amd64.exe",
            ),
            (
                "Windows installer (64-bit)",
                "https://evil.invalid/ftp/python/3.10.11/python-3.10.11-amd64.exe",
            ),
            (
                "Windows installer (64-bit)",
                "https://www.python.org/ftp/python/3.10.10/python-3.10.10-amd64.exe",
            ),
            (
                "Windows installer (64-bit)",
                "https://www.python.org/ftp/python/3.10.11/python-3.10.11-embed-amd64.zip",
            ),
            (
                "Windows installer (64-bit)",
                "https://www.python.org/ftp/python/3.10.11/python-3.10.11-amd64-webinstall.exe",
            ),
            (
                "Windows installer (64-bit)",
                "http://www.python.org:8080/ftp/python/3.10.11/python-3.10.11-amd64.exe",
            ),
            (
                "Windows installer (64-bit)",
                "https://user@www.python.org/ftp/python/3.10.11/python-3.10.11-amd64.exe",
            ),
        ] {
            let mut file = python_installer_fixture(version, "x86_64");
            file.name = name.into();
            file.url = url.into();
            assert!(python_installer_file(version, &file).is_none(), "{url}");
        }
        let mut historical = python_installer_fixture("3.3.4", "x86_64");
        historical.url = "http://www.python.org/ftp/python/3.3.4/python-3.3.4.amd64.msi".into();
        assert_eq!(
            python_installer_file("3.3.4", &historical).unwrap().0,
            "https://www.python.org/ftp/python/3.3.4/python-3.3.4.amd64.msi"
        );
        let mut short = python_installer_fixture("3.0.0", "x86_64");
        short.url = "https://www.python.org/ftp/python/3.0/python-3.0.amd64.msi".into();
        assert!(python_installer_file("3.0.0", &short).is_some());
    }

    #[test]
    fn python_history_keeps_unverified_rows_and_supports_verified_wise_installers() {
        let mut unsigned = python_installer_fixture("2.7.18", "x86_64");
        unsigned.md5_sum.clear();
        let mut old_exe = python_installer_fixture("2.3.5", "x86");
        old_exe.name = "Windows installer".into();
        old_exe.url = "https://www.python.org/ftp/python/2.3.5/Python-2.3.5.exe".into();
        let versions = build_python_history(
            vec![
                python_release_fixture("2.7.18", false),
                python_release_fixture("2.3.5", false),
            ],
            vec![unsigned, old_exe],
            vec![],
            PYTHON_LEGACY_FIXTURE,
            PYTHON_FTP_FIXTURE,
        )
        .unwrap();
        assert!(
            versions
                .iter()
                .find(|remote| remote.version == "2.7.18")
                .unwrap()
                .install_unavailable_reason
                .as_ref()
                .unwrap()
                .contains("校验值")
        );
        let wise = versions
            .iter()
            .find(|remote| remote.version == "2.3.5")
            .unwrap();
        assert!(wise.install_unavailable_reason.is_none());
        assert!(is_supported_python_runtime(wise));
        assert!(
            wise.download_url
                .as_deref()
                .unwrap()
                .ends_with("Python-2.3.5.exe")
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut blocked = versions
            .into_iter()
            .find(|remote| remote.version == "2.7.18")
            .unwrap();
        let error = runtime
            .block_on(ensure_install_metadata(
                &local_client(),
                "python",
                &mut blocked,
            ))
            .unwrap_err();
        assert!(error.to_string().contains("校验值"));
    }

    #[test]
    fn python_history_uses_official_sha256_and_does_not_hide_malformed_hashes_with_md5() {
        let mut file = python_installer_fixture("2.7.18", "x86_64");
        file.sha256_sum = "B".repeat(64);
        assert_eq!(
            python_file_checksum(&file),
            Some(("sha256".into(), "b".repeat(64)))
        );
        file.sha256_sum = "invalid official hash".into();
        assert!(python_file_checksum(&file).is_none());
        file.sha256_sum.clear();
        file.md5_sum = "A".repeat(32);
        assert_eq!(
            python_file_checksum(&file),
            Some(("md5".into(), "a".repeat(32)))
        );
        file.md5_sum = "invalid".into();
        assert!(python_file_checksum(&file).is_none());
    }

    #[test]
    fn python_history_rejects_conflicting_hashes_and_old_or_corrupted_cache() {
        let first = python_installer_fixture("2.7.18", "x86_64");
        let mut conflicting = python_installer_fixture("2.7.18", "x86_64");
        conflicting.md5_sum = "b".repeat(32);
        let error = build_python_history(
            vec![python_release_fixture("2.7.18", false)],
            vec![first, conflicting],
            vec![],
            PYTHON_LEGACY_FIXTURE,
            PYTHON_FTP_FIXTURE,
        )
        .unwrap_err();
        assert!(error.to_string().contains("冲突"));
        let full = python_runtime_version(python_runtime_fixture("3.15.0")).unwrap();
        let old = catalog(
            "python",
            "Python.org 完整开发运行时",
            PYTHON_RUNTIME_SOURCE,
            vec![full.clone()],
        );
        assert!(!is_complete_python_catalog(&old));
        let mut current = catalog(
            "python",
            PYTHON_HISTORY_NAME,
            PYTHON_RELEASE_SOURCE,
            vec![full],
        );
        assert!(is_complete_python_catalog(&current));
        current.versions[0].download_url =
            Some("https://evil.invalid/python-3.15.0-amd64.zip".into());
        assert!(!is_complete_python_catalog(&current));
        current.versions[0].download_url = None;
        current.versions[0].install_unavailable_reason = Some("no file".into());
        assert!(!is_complete_python_catalog(&current));
        assert!(
            build_python_history(
                vec![python_release_fixture("2.7.18", false)],
                vec![],
                vec![],
                "unrecognized HTML",
                PYTHON_FTP_FIXTURE
            )
            .is_err()
        );
    }

    fn python_runtime_fixture(version: &str) -> PythonRuntime {
        let minor = version.split('.').nth(1).unwrap();
        PythonRuntime {
            schema: 1,
            id: format!("pythoncore-3.{minor}-64"),
            company: "PythonCore".to_string(),
            tag: format!("3.{minor}-64"),
            version: version.to_string(),
            url: format!("https://www.python.org/ftp/python/{version}/python-{version}-amd64.zip"),
            hash: Some(HashMap::from([("sha256".to_string(), "a".repeat(64))])),
        }
    }

    #[test]
    fn python_catalog_keeps_every_complete_x64_runtime_and_rejects_embed_or_unverified_packages() {
        let versions = (0..40)
            .filter_map(|index| {
                python_runtime_version(python_runtime_fixture(&format!(
                    "3.{}.{}",
                    11 + index / 10,
                    index % 10
                )))
            })
            .collect::<Vec<_>>();
        assert_eq!(versions.len(), 40);
        assert!(versions.iter().all(is_supported_python_runtime));
        let mut embedded = python_runtime_fixture("3.15.0");
        embedded.company = "PythonEmbed".into();
        let mut old_embedded = python_runtime_fixture("3.15.0");
        old_embedded.url =
            "https://www.python.org/ftp/python/3.15.0/python-3.15.0-embed-amd64.zip".into();
        let mut preview = python_runtime_fixture("3.15.0rc1");
        preview.tag = "3.15-64".into();
        let mut freethreaded = python_runtime_fixture("3.15.0");
        freethreaded.tag = "3.15t-64".into();
        let mut arm64 = python_runtime_fixture("3.15.0");
        arm64.tag = "3.15-arm64".into();
        let mut wrong_version_url = python_runtime_fixture("3.15.0");
        wrong_version_url.url =
            "https://www.python.org/ftp/python/3.14.0/python-3.14.0-amd64.zip".into();
        let mut unverified = python_runtime_fixture("3.15.0");
        unverified.hash = None;
        let mut invalid_hash = python_runtime_fixture("3.15.0");
        invalid_hash.hash = Some(HashMap::from([("sha256".into(), "bad".into())]));
        let mut legacy = python_runtime_fixture("3.10.11");
        legacy.url =
            "https://api.nuget.org/v3-flatcontainer/python/3.10.11/python.3.10.11.nupkg".into();
        legacy.hash = None;
        for invalid in [
            embedded,
            old_embedded,
            preview,
            freethreaded,
            arm64,
            wrong_version_url,
            unverified,
            invalid_hash,
            legacy,
        ] {
            assert!(python_runtime_version(invalid).is_none());
        }
    }

    #[test]
    fn python_index_pagination_preserves_full_history_without_per_version_requests() {
        let mut legacy = python_runtime_fixture("3.10.11");
        legacy.hash = None;
        let mut pages = std::collections::VecDeque::from([
            PythonRuntimeIndex {
                versions: vec![python_runtime_fixture("3.15.0")],
                next: Some("index-windows-recent.json".into()),
            },
            PythonRuntimeIndex {
                versions: vec![
                    python_runtime_fixture("3.15.0"),
                    python_runtime_fixture("3.11.0"),
                ],
                next: Some("index-windows-legacy.json".into()),
            },
            PythonRuntimeIndex {
                versions: vec![legacy],
                next: None,
            },
        ]);
        let mut requests = Vec::new();
        let versions = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fetch_python_indexes(|url| {
                requests.push(url);
                let page = pages
                    .pop_front()
                    .expect("only the index chain should be fetched");
                async move { Ok(page) }
            }))
            .unwrap();
        assert_eq!(
            requests,
            [
                PYTHON_RUNTIME_SOURCE,
                "https://www.python.org/ftp/python/index-windows-recent.json",
                "https://www.python.org/ftp/python/index-windows-legacy.json",
            ]
        );
        assert_eq!(versions.len(), 2);
        assert!(versions.iter().any(|remote| remote.version == "3.11.0"));
        assert!(versions.iter().all(is_supported_python_runtime));
    }

    #[test]
    fn python_index_rejects_foreign_pagination_cycles_and_conflicting_checksums() {
        for next in [
            "https://evil.invalid/index-windows.json",
            "https://www.python.org/other/index-windows.json",
            "https://www.python.org/ftp/python/index-windows.json?redirect=1",
            "https://user@www.python.org/ftp/python/index-windows.json",
            "http://www.python.org/ftp/python/index-windows.json",
        ] {
            assert!(
                python_next_index(PYTHON_RUNTIME_SOURCE, next).is_err(),
                "{next}"
            );
        }
        let mut requests = 0;
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fetch_python_indexes(|_| {
                requests += 1;
                async {
                    Ok(PythonRuntimeIndex {
                        versions: vec![python_runtime_fixture("3.15.0")],
                        next: Some("index-windows.json".into()),
                    })
                }
            }))
            .unwrap_err();
        assert!(error.to_string().contains("循环"));
        assert_eq!(requests, 1);
        let mut conflicting = python_runtime_fixture("3.15.0");
        conflicting.hash = Some(HashMap::from([("sha256".into(), "b".repeat(64))]));
        let mut pages = std::collections::VecDeque::from([
            PythonRuntimeIndex {
                versions: vec![python_runtime_fixture("3.15.0")],
                next: Some("index-windows-recent.json".into()),
            },
            PythonRuntimeIndex {
                versions: vec![conflicting],
                next: None,
            },
        ]);
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fetch_python_indexes(|_| {
                let page = pages.pop_front().unwrap();
                async move { Ok(page) }
            }))
            .unwrap_err();
        assert!(error.to_string().contains("冲突"));
    }

    #[test]
    fn selected_python_runtime_rejects_embedded_cache_without_network_and_keeps_sha256() {
        let mut runtime = python_runtime_version(python_runtime_fixture("3.15.0")).unwrap();
        let executor = tokio::runtime::Runtime::new().unwrap();
        let checksum = runtime.checksum.clone();
        executor
            .block_on(ensure_install_metadata(
                &local_client(),
                "python",
                &mut runtime,
            ))
            .unwrap();
        assert_eq!(runtime.checksum, checksum);
        runtime.download_url =
            Some("https://www.python.org/ftp/python/3.15.0/python-3.15.0-embed-amd64.zip".into());
        assert!(!is_supported_python_runtime(&runtime));
        let error = executor
            .block_on(ensure_install_metadata(
                &local_client(),
                "python",
                &mut runtime,
            ))
            .unwrap_err();
        assert!(error.to_string().contains("刷新版本列表"));
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
                    let downloads = catalog
                        .versions
                        .iter()
                        .filter(|version| version.download_url.is_some())
                        .count();
                    println!(
                        "{tool_id}: {} versions, {downloads} Windows downloads",
                        catalog.versions.len()
                    );
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
