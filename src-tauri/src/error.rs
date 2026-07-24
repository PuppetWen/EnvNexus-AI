use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("I/O 操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("网络请求失败：{0}")]
    Http(#[from] reqwest::Error),
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
