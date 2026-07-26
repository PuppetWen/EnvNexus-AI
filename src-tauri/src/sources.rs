use std::collections::{HashMap, HashSet};

use chrono::Utc;
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
    match kind {
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
                "https://api.github.com/repos/git-for-windows/git/releases?per_page=20",
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
                "https://api.github.com/repos/Kitware/CMake/releases?per_page=20",
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
    }
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
    let response = checked_response(client, url).await?;
    response.text().await.map_err(AppError::from)
}

async fn checked_json<T>(client: &reqwest::Client, url: &str) -> AppResult<T>
where
    T: serde::de::DeserializeOwned,
{
    let response = checked_response(client, url).await?;
    response.json::<T>().await.map_err(AppError::from)
}

async fn checked_response(client: &reqwest::Client, url: &str) -> AppResult<reqwest::Response> {
    for attempt in 0..2 {
        match client.get(url).send().await {
            Ok(response) => match response.error_for_status() {
                Ok(response) => return Ok(response),
                Err(error)
                    if attempt == 0
                        && error.status().is_some_and(|status| {
                            status.is_server_error() || status.as_u16() == 429
                        }) =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(error) => return Err(error.into()),
            },
            Err(error) if attempt == 0 && (error.is_timeout() || error.is_connect()) => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(AppError::Message(format!(
        "官方版本源重试后仍不可用：{url}"
    )))
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
    url: String,
    sha256_sum: String,
}

async fn fetch_python(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://www.python.org/api/v2/downloads/release/?is_published=true";
    let mut releases = checked_json::<Vec<PythonRelease>>(client, SOURCE).await?;
    releases.retain(|release| release.name.starts_with("Python 3.") && !release.pre_release);
    releases.sort_by(|left, right| right.release_date.cmp(&left.release_date));
    releases.truncate(18);

    let client = client.clone();
    let results = stream::iter(releases)
        .map(move |release| {
            let client = client.clone();
            async move {
                let id = release
                    .resource_uri
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .ok_or_else(|| AppError::InvalidSource("Python release id 缺失".to_string()))?;
                let url =
                    format!("https://www.python.org/api/v2/downloads/release_file/?release={id}");
                let files = checked_json::<Vec<PythonReleaseFile>>(&client, &url).await?;
                let file = files.into_iter().find(|file| {
                    file.name
                        .to_ascii_lowercase()
                        .contains("windows embeddable package (64-bit)")
                });
                Ok::<_, AppError>(file.map(|file| RemoteVersion {
                    version: release.name.trim_start_matches("Python ").to_string(),
                    channel: "stable".to_string(),
                    published_at: Some(release.release_date),
                    architecture: "x86_64".to_string(),
                    download_url: Some(file.url),
                    checksum_algorithm: non_empty("sha256"),
                    checksum: non_empty(&file.sha256_sum),
                    notes_url: release.release_notes_url,
                }))
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;

    let mut versions = results
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| right.published_at.cmp(&left.published_at));
    Ok(catalog(tool_id, "Python.org", SOURCE, versions))
}

#[derive(Debug, Deserialize)]
struct AdoptiumInfo {
    available_lts_releases: Vec<u32>,
    most_recent_feature_release: u32,
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
    // LTS 以官方 available_lts_releases 集合为准（8、11、17、21、25…），
    // 不能用奇偶数推断。
    let lts_releases = info
        .available_lts_releases
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut majors = info.available_lts_releases;
    majors.push(info.most_recent_feature_release);
    majors.sort_unstable_by(|left, right| right.cmp(left));
    majors.dedup();
    majors.truncate(6);

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
    let mut versions = results
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .collect::<Vec<_>>();
    // buffer_unordered 按完成顺序返回，需要重新按版本号降序排列
    versions.sort_by(|left, right| crate::scanner::compare_versions(&right.version, &left.version));
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
        .take(30)
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
    // 源按版本严格降序；直接取前 N 条只覆盖最新两三个大版本，
    // 会漏掉仍在维护期的旧 LTS 线。改为最新 8 个大版本、每线取前 3 个，
    // 请求量与原上限相当。
    let mut majors_seen = Vec::<u64>::new();
    let mut per_major = HashMap::<u64, usize>::new();
    let candidates = releases
        .into_iter()
        .filter(|release| release.files.iter().any(|file| file == "win-x64-zip"))
        .filter(|release| {
            let major = release
                .version
                .trim_start_matches('v')
                .split('.')
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            if !majors_seen.contains(&major) {
                if majors_seen.len() >= 8 {
                    return false;
                }
                majors_seen.push(major);
            }
            let count = per_major.entry(major).or_insert(0);
            *count += 1;
            *count <= 3
        })
        .collect::<Vec<_>>();
    let client = client.clone();
    let results = stream::iter(candidates)
        .map(move |release| {
            let client = client.clone();
            async move {
                let filename = format!("node-{}-win-x64.zip", release.version);
                let checksum_url =
                    format!("https://nodejs.org/dist/{}/SHASUMS256.txt", release.version);
                let checksum = checked_text(&client, &checksum_url)
                    .await
                    .ok()
                    .and_then(|text| {
                        text.lines().find_map(|line| {
                            let mut parts = line.split_whitespace();
                            let hash = parts.next()?;
                            let name = parts.next()?.trim_start_matches('*');
                            (name == filename).then(|| hash.to_string())
                        })
                    });
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
                    checksum_algorithm: checksum.as_ref().map(|_| "sha256".to_string()),
                    checksum,
                    notes_url: Some(format!(
                        "https://nodejs.org/en/blog/release/{}",
                        release.version
                    )),
                }
            }
        })
        .buffer_unordered(5)
        .collect::<Vec<_>>()
        .await;
    let mut versions = results;
    versions.sort_by(|left, right| right.published_at.cmp(&left.published_at));
    Ok(catalog(tool_id, "Node.js Releases", SOURCE, versions))
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
        .take(30)
        .collect();
    Ok(catalog(tool_id, "Gradle Services", SOURCE, versions))
}

async fn fetch_maven(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str = "https://downloads.apache.org/maven/maven-3/";
    let html = checked_text(client, SOURCE).await?;
    let version_pattern = Regex::new(r#"href="([0-9]+\.[0-9]+\.[0-9]+)/""#)
        .map_err(|error| AppError::Message(error.to_string()))?;
    // 目录页按版本升序排列；先按版本降序再截断，避免只保留最旧的版本
    let mut candidates = version_pattern
        .captures_iter(&html)
        .filter_map(|capture| capture.get(1).map(|value| value.as_str().to_string()))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| crate::scanner::compare_versions(right, left));
    candidates.dedup();
    candidates.truncate(12);
    let client = client.clone();
    let mut versions = stream::iter(candidates)
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
    // buffer_unordered 按完成顺序返回，需要重新按版本号降序排列
    versions.sort_by(|left, right| crate::scanner::compare_versions(&right.version, &left.version));
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
    channels.truncate(6);
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
                    .take(3)
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
    versions.truncate(24);
    Ok(catalog(
        tool_id,
        "Microsoft .NET Releases",
        SOURCE,
        versions,
    ))
}

async fn fetch_ruby(client: &reqwest::Client, tool_id: &str) -> AppResult<VersionCatalog> {
    const SOURCE: &str =
        "https://api.github.com/repos/oneclick/rubyinstaller2/releases?per_page=30";
    let releases = checked_json::<Vec<GithubRelease>>(client, SOURCE).await?;
    let versions = releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let asset = release.assets.iter().find(|asset| {
                asset.name.starts_with("rubyinstaller-")
                    && asset.name.ends_with("-x64.7z")
                    && !asset.name.contains("devkit")
            })?;
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
        .take(24)
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
    let mut versions = file_pattern
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
        .collect::<Vec<_>>();
    // 目录页按版本升序排列；统一为新→旧再截断
    versions.sort_by(|left, right| crate::scanner::compare_versions(&right.version, &left.version));
    versions.truncate(24);
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
    let releases = checked_json::<Vec<GithubRelease>>(client, source_url).await?;
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
        .take(20)
        .collect();
    Ok(catalog(tool_id, source_name, source_url, versions))
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
        if versions.len() >= 30 {
            break;
        }
    }
    Ok(versions)
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
