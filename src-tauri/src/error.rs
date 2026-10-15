use std::{error::Error as StdError, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("I/O 操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("网络请求失败：{}", describe_http_error(.0))]
    Http(#[from] reqwest::Error),
    #[error("{}", describe_source_http(*.status, .url, .retry_at.as_ref(), *.rate_limited))]
    SourceHttp {
        status: u16,
        url: String,
        retry_at: Option<chrono::DateTime<chrono::Utc>>,
        rate_limited: bool,
    },
    #[error("JSON 解析失败：{0}")]
    Json(#[from] serde_json::Error),
    #[error("配置解析失败：{0}")]
    Toml(#[from] toml::de::Error),
    #[error("不支持的工具：{0}")]
    UnknownTool(String),
    #[error("官方版本源返回了无法识别的数据：{0}")]
    InvalidSource(String),
    #[error("路径不在允许的受管根目录中：{0}")]
    UnsafePath(PathBuf),
    #[error("操作计划不存在、已过期或已被使用")]
    InvalidPlan,
    #[error("确认令牌不匹配")]
    ConfirmationMismatch,
    #[error("确认后环境已发生变化，请重新预览")]
    StaleEnvironment,
    #[error("该操作需要系统级权限，EnvNexus AI 默认拒绝执行")]
    SystemScopeDenied,
    #[error("下载校验失败：期望 {expected}，实际 {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("{0}")]
    Message(String),
}

pub type AppResult<T> = Result<T, AppError>;

pub fn command_error(error: AppError) -> String {
    error.to_string()
}

fn describe_source_http(
    status: u16,
    url: &str,
    retry_at: Option<&chrono::DateTime<chrono::Utc>>,
    rate_limited: bool,
) -> String {
    let reason = if rate_limited {
        "官方服务的请求限额已用尽，当前出口 IP 可能与其他代理用户共享限额".to_string()
    } else {
        match status {
            401 => "官方服务要求身份验证".to_string(),
            403 => "官方服务拒绝了请求".to_string(),
            404 => "官方服务中不存在该版本或地址".to_string(),
            500..=599 => "官方服务暂时不可用".to_string(),
            _ => "官方服务未接受请求".to_string(),
        }
    };
    let retry = retry_at
        .map(|at| format!("；可在 {} 后重试", at.format("%Y-%m-%d %H:%M:%S UTC")))
        .unwrap_or_else(|| {
            if rate_limited {
                "；服务未提供恢复时间，请稍后重试".to_string()
            } else {
                String::new()
            }
        });
    format!("官方版本源请求失败（HTTP {status}）：{reason}{retry}；{url}")
}

fn describe_http_error(error: &reqwest::Error) -> String {
    let mut causes = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        let detail = cause.to_string();
        if !detail.is_empty() && causes.last() != Some(&detail) {
            causes.push(detail);
        }
        source = cause.source();
    }
    let details = causes.join("；原因：");
    let lower = details.to_ascii_lowercase();
    let context = if error.is_timeout() {
        "请求超时，请检查网络或代理连接"
    } else if lower.contains("dns") || lower.contains("lookup address") {
        "DNS 解析失败，请检查 DNS 或代理设置"
    } else if lower.contains("proxy") || lower.contains("tunnel") {
        "代理连接失败，请检查代理地址与运行状态"
    } else if lower.contains("certificate") || lower.contains("tls") {
        "HTTPS 连接验证失败，请检查证书或代理设置"
    } else if error.is_connect() {
        "无法连接服务器，请检查网络、DNS 或代理"
    } else if error.is_body() {
        "下载连接中断，可重试继续下载"
    } else {
        "服务器请求失败"
    };

    // Signed release redirects and proxy credentials must not enter journals.
    let details = if let Some(url) = error.url() {
        let mut safe_url = url.clone();
        let _ = safe_url.set_username("");
        let _ = safe_url.set_password(None);
        safe_url.set_query(None);
        safe_url.set_fragment(None);
        details.replace(url.as_str(), safe_url.as_str())
    } else {
        details
    };
    format!("{context}：{details}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };

    #[test]
    fn connection_errors_include_underlying_cause() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = tokio::runtime::Runtime::new().unwrap().block_on(async {
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(format!("http://{address}/package.zip"))
                .send()
                .await
                .unwrap_err()
        });
        let message = AppError::Http(error).to_string();
        assert!(message.contains("无法连接服务器"));
        assert!(message.contains("原因："));
        assert!(message.contains("package.zip"));
    }

    #[test]
    fn timeout_errors_explain_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });
        let error = tokio::runtime::Runtime::new().unwrap().block_on(async {
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_millis(50))
                .build()
                .unwrap()
                .get(format!("http://{address}/package.zip"))
                .send()
                .await
                .unwrap_err()
        });
        server.join().unwrap();
        assert!(AppError::Http(error).to_string().contains("请求超时"));
    }

    #[test]
    fn http_errors_do_not_expose_signed_query_parameters() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let error = tokio::runtime::Runtime::new().unwrap().block_on(async {
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(format!("http://{address}/package.zip?token=private-value"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap_err()
        });
        server.join().unwrap();
        let message = AppError::Http(error).to_string();
        assert!(message.contains("403"));
        assert!(message.contains("package.zip"));
        assert!(!message.contains("private-value"));
    }
}
