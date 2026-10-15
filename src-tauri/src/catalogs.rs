use std::{collections::HashMap, future::Future, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::{Duration, Instant}};

use chrono::Utc;
use tokio::sync::Mutex as AsyncMutex;

use crate::{error::{AppError, AppResult}, model::{RemoteVersion, VersionCatalog}, plugins::PluginRegistry};

const FRESH_FOR_SECONDS: i64 = 15 * 60;
const QUERY_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Attempt {
    completed_at: Option<Instant>,
    result: Option<Result<VersionCatalog, String>>,
}

type QueryKey = (PathBuf, String);
type QueryGate = Arc<AsyncMutex<Attempt>>;

/// Shares in-flight queries and remembers failures so repeated clicks do not
/// multiply requests against a rate-limited official service.
#[derive(Default)]
pub struct CatalogService {
    queries: Mutex<HashMap<QueryKey, QueryGate>>,
}

pub fn needs_refresh(catalog: &VersionCatalog) -> bool {
    let age = Utc::now().signed_duration_since(catalog.fetched_at).num_seconds();
    !(-300..FRESH_FOR_SECONDS).contains(&age)
}

impl CatalogService {
    fn query_gate(&self, root: &Path, tool_id: &str) -> AppResult<QueryGate> {
        Ok(self.queries.lock().map_err(|_| AppError::Message("版本查询状态不可用".to_string()))?
            .entry((root.to_path_buf(), tool_id.to_string())).or_default().clone())
    }

    pub async fn load(&self, registry: &PluginRegistry, root: &Path, tool_id: &str, force: bool) -> AppResult<VersionCatalog> {
        let plugin = registry.get(tool_id)?;
        self.fetch_with(root, tool_id, force, || async {
            // Re-read Windows proxy settings at the start of each actual query.
            let client = crate::metadata_http_client()?;
            tokio::time::timeout(Duration::from_secs(30), plugin.fetch_available_versions(&client))
                .await
                .map_err(|_| AppError::Message("官方版本查询超过 30 秒，请稍后重试".to_string()))?
        }).await
    }

    async fn fetch_with<F, Fut>(&self, root: &Path, tool_id: &str, force: bool, fetch: F) -> AppResult<VersionCatalog>
    where F: FnOnce() -> Fut, Fut: Future<Output = AppResult<VersionCatalog>> {
        let cached = crate::read_version_catalog_cache(root, tool_id).ok().flatten();
        if !force && let Some(catalog) = cached.as_ref().filter(|catalog| !needs_refresh(catalog)) {
            return Ok(catalog.clone());
        }
        let gate = self.query_gate(root, tool_id)?;
        let mut attempt = gate.lock().await;
        if attempt.completed_at.is_some_and(|time| time.elapsed() < QUERY_COOLDOWN)
            && let Some(result) = &attempt.result {
            return match result {
                Ok(catalog) => { let mut catalog = catalog.clone(); catalog.cached = true; Ok(catalog) }
                Err(message) => cached.ok_or_else(|| AppError::Message(message.clone())),
            };
        }
        let result = fetch().await.and_then(|mut catalog| {
            if catalog.tool_id != tool_id || catalog.versions.is_empty() {
                return Err(AppError::InvalidSource("官方清单为空或工具 ID 不匹配".to_string()));
            }
            crate::versioning::sort_remote_versions_descending(&mut catalog.versions);
            catalog.cached = false;
            let _ = crate::write_version_catalog_cache(root, &catalog);
            Ok(catalog)
        });
        attempt.completed_at = Some(Instant::now());
        attempt.result = Some(result.as_ref().map(Clone::clone).map_err(ToString::to_string));
        match result {
            Ok(catalog) => Ok(catalog),
            Err(error) => cached.ok_or(error),
        }
    }

    pub async fn selected_version(&self, registry: &PluginRegistry, root: &Path, tool_id: &str, version: &str) -> AppResult<RemoteVersion> {
        registry.get(tool_id)?;
        let mut catalog = match crate::read_version_catalog_cache(root, tool_id).ok().flatten() {
            Some(catalog) if catalog.versions.iter().any(|remote| remote.version == version) => catalog,
            _ => self.load(registry, root, tool_id, false).await?,
        };
        if !catalog.versions.iter().any(|remote| remote.version == version) {
            catalog = self.load(registry, root, tool_id, true).await?;
        }
        let snapshot = catalog.versions.iter().find(|remote| remote.version == version)
            .cloned().ok_or_else(|| AppError::Message("所选版本不在官方版本清单中，请刷新版本列表".to_string()))?;
        // Metadata hydration and background refresh share the same write gate.
        let gate = self.query_gate(root, tool_id)?;
        let _guard = gate.lock().await;
        catalog = crate::read_version_catalog_cache(root, tool_id).ok().flatten().unwrap_or(catalog);
        if !catalog.versions.iter().any(|remote| remote.version == version) {
            catalog.versions.push(snapshot);
        }
        let remote = catalog.versions.iter_mut().find(|remote| remote.version == version)
            .ok_or_else(|| AppError::Message("所选版本不在官方版本清单中，请刷新版本列表".to_string()))?;
        let client = crate::metadata_http_client()?;
        tokio::time::timeout(Duration::from_secs(30), crate::sources::ensure_install_metadata(&client, tool_id, remote)).await
            .map_err(|_| AppError::Message("所选版本的校验信息查询超过 30 秒，请稍后重试".to_string()))??;
        let selected = remote.clone();
        let _ = crate::write_version_catalog_cache(root, &catalog);
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture(age: i64) -> VersionCatalog {
        VersionCatalog { tool_id: "node".into(), source_name: "Node.js".into(), source_url: "https://nodejs.org/dist/index.json".into(), fetched_at: Utc::now() - chrono::Duration::seconds(age), cached: false, versions: vec![RemoteVersion { version: "24.0.0".into(), channel: "LTS".into(), published_at: None, architecture: "x86_64".into(), download_url: Some("https://nodejs.org/dist/v24.0.0/node-v24.0.0-win-x64.zip".into()), checksum_algorithm: None, checksum: None, notes_url: None }] }
    }

    #[test]
    fn fresh_catalog_needs_no_network_and_force_refresh_is_coalesced() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = tempfile::tempdir().unwrap();
        crate::write_version_catalog_cache(root.path(), &fixture(0)).unwrap();
        let service = CatalogService::default();
        let cached = service.fetch_with(root.path(), "node", false, || async { panic!("fresh cache must avoid network") }).await.unwrap();
        assert!(cached.cached);
        let calls = AtomicUsize::new(0);
        for _ in 0..2 {
            service.fetch_with(root.path(), "node", true, || async { calls.fetch_add(1, Ordering::SeqCst); Ok(fixture(0)) }).await.unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn stale_cache_survives_failure_and_repeated_failure_is_not_requested_again() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = tempfile::tempdir().unwrap();
        crate::write_version_catalog_cache(root.path(), &fixture(3600)).unwrap();
        let service = CatalogService::default();
        let cached = service.fetch_with(root.path(), "node", false, || async { Err(AppError::Message("HTTP 403: rate limited".into())) }).await.unwrap();
        assert!(cached.cached);
        let cached = service.fetch_with(root.path(), "node", true, || async { panic!("cooldown must prevent a second request") }).await.unwrap();
        assert!(cached.cached);
        });
    }

    #[test]
    fn failure_without_cache_keeps_precise_error_and_stops_repeat_queries() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let service = CatalogService::default();
        let error = service.fetch_with(root.path(), "node", false, || async { Err(AppError::Message("GitHub HTTP 403: reset at 12:00 UTC".into())) }).await.unwrap_err();
        assert!(error.to_string().contains("reset at 12:00 UTC"));
        let error = service.fetch_with(root.path(), "node", true, || async { panic!("cooldown must prevent a second request") }).await.unwrap_err();
        assert!(error.to_string().contains("HTTP 403"));
        });
    }

    #[test]
    fn concurrent_clicks_share_one_official_query() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let service = CatalogService::default();
        let calls = AtomicUsize::new(0);
        let first = service.fetch_with(root.path(), "node", true, || async { calls.fetch_add(1, Ordering::SeqCst); tokio::task::yield_now().await; Ok(fixture(0)) });
        let second = service.fetch_with(root.path(), "node", true, || async { calls.fetch_add(1, Ordering::SeqCst); Ok(fixture(0)) });
        let (first, second) = futures_util::future::join(first, second).await;
        assert!(first.is_ok() && second.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }
}
