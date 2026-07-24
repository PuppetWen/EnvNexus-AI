use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    error::{AppError, AppResult},
    model::{
        AiDiagnosticAnalysis, AiModelInfo, AiProviderConfig, AiProviderInput, AiSettings,
        DiagnosticGuidance, DiagnosticIssue, EnvironmentScan, MachineContext,
        VersionManagerInventory,
    },
};

const SETTINGS_SCHEMA: u32 = 1;
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacySecretStore {
    schema_version: u32,
    encrypted_keys: BTreeMap<String, String>,
}

impl Default for LegacySecretStore {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA,
            encrypted_keys: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderDocument {
    schema_version: u32,
    provider: AiProviderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderSecretDocument {
    schema_version: u32,
    encrypted_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActiveProviderDocument {
    schema_version: u32,
    active_provider_id: Option<String>,
}

pub fn read_settings(data_root: &Path) -> AppResult<AiSettings> {
    migrate_legacy_storage(data_root)?;
    let active = read_active_provider(data_root)?;
    let mut providers = read_provider_documents(data_root)?;
    merge_builtin_providers(&mut providers);
    for provider in &mut providers {
        provider.api_key_configured = provider_secret_path(data_root, &provider.id).is_file();
    }
    Ok(AiSettings {
        schema_version: SETTINGS_SCHEMA,
        active_provider_id: active.active_provider_id,
        providers,
    })
}

pub fn save_provider(data_root: &Path, input: AiProviderInput) -> AppResult<AiSettings> {
    validate_provider_id(&input.id)?;
    let protocol = normalize_protocol(&input.protocol)?;
    let base_url = normalize_base_url(&input.base_url)?;
    read_settings(data_root)?;
    let builtin = builtin_provider(&input.id).is_some();
    let provider = AiProviderConfig {
        id: input.id.clone(),
        display_name: input.display_name.trim().to_string(),
        protocol,
        base_url,
        selected_model: input
            .selected_model
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty()),
        api_key_configured: false,
        builtin,
    };
    if provider.display_name.is_empty() {
        return Err(AppError::Message("AI 厂商名称不能为空".to_string()));
    }
    write_provider_document(data_root, &provider)?;

    if let Some(api_key) = input.api_key {
        let api_key = api_key.trim();
        if !api_key.is_empty() {
            let secret = ProviderSecretDocument {
                schema_version: SETTINGS_SCHEMA,
                encrypted_key: protect_secret(api_key.as_bytes())?,
            };
            write_json_atomic(&provider_secret_path(data_root, &input.id), &secret)?;
        }
    }
    read_settings(data_root)
}

pub fn clear_api_key(data_root: &Path, provider_id: &str) -> AppResult<AiSettings> {
    validate_provider_id(provider_id)?;
    let path = provider_secret_path(data_root, provider_id);
    if path.is_file() {
        fs::remove_file(path)?;
    }
    read_settings(data_root)
}

pub fn select_model(data_root: &Path, provider_id: &str, model: &str) -> AppResult<AiSettings> {
    let settings = read_settings(data_root)?;
    let mut provider = settings
        .providers
        .into_iter()
        .find(|candidate| candidate.id == provider_id)
        .ok_or_else(|| AppError::Message("AI 厂商配置不存在".to_string()))?;
    let model = model.trim();
    if model.is_empty() {
        return Err(AppError::Message("模型 ID 不能为空".to_string()));
    }
    provider.selected_model = Some(model.to_string());
    write_provider_document(data_root, &provider)?;
    read_settings(data_root)
}

pub fn activate_provider(data_root: &Path, provider_id: &str) -> AppResult<AiSettings> {
    validate_provider_id(provider_id)?;
    let settings = read_settings(data_root)?;
    let provider = settings
        .providers
        .iter()
        .find(|candidate| candidate.id == provider_id)
        .ok_or_else(|| AppError::Message("AI 厂商配置不存在".to_string()))?;
    if !provider.api_key_configured {
        return Err(AppError::Message(
            "该 AI 厂商尚未保存 API Key，不能设为当前厂商".to_string(),
        ));
    }
    if provider.selected_model.is_none() {
        return Err(AppError::Message(
            "该 AI 厂商尚未选择模型，不能设为当前厂商".to_string(),
        ));
    }
    write_json_atomic(
        &active_provider_path(data_root),
        &ActiveProviderDocument {
            schema_version: SETTINGS_SCHEMA,
            active_provider_id: Some(provider_id.to_string()),
        },
    )?;
    read_settings(data_root)
}

pub async fn fetch_models(
    client: &reqwest::Client,
    data_root: &Path,
    provider_id: &str,
) -> AppResult<Vec<AiModelInfo>> {
    let settings = read_settings(data_root)?;
    let provider = settings
        .providers
        .iter()
        .find(|candidate| candidate.id == provider_id)
        .ok_or_else(|| AppError::Message("AI 厂商配置不存在".to_string()))?;
    let api_key = read_api_key(data_root, provider_id)?;
    let endpoint = format!("{}/models", provider.base_url.trim_end_matches('/'));
    let request = match provider.protocol.as_str() {
        "anthropic" => client
            .get(endpoint)
            .header("x-api-key", &api_key)
            .header("anthropic-version", ANTHROPIC_VERSION),
        "gemini" => client.get(endpoint).header("x-goog-api-key", &api_key),
        _ => client.get(endpoint).bearer_auth(&api_key),
    };
    let value = checked_json(request).await?;
    let mut models = match provider.protocol.as_str() {
        "gemini" => parse_gemini_models(&value),
        _ => parse_data_models(&value),
    }?;
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    if models.is_empty() {
        return Err(AppError::Message(
            "厂商返回了空模型列表；请核对 URL、密钥权限和 API 协议".to_string(),
        ));
    }
    Ok(models)
}

pub async fn analyze_diagnostic(
    client: &reqwest::Client,
    data_root: &Path,
    issue: &DiagnosticIssue,
    managers: &[VersionManagerInventory],
    machine: &MachineContext,
    guidance: &DiagnosticGuidance,
    scan: &EnvironmentScan,
) -> AppResult<AiDiagnosticAnalysis> {
    let settings = read_settings(data_root)?;
    let provider_id = settings
        .active_provider_id
        .as_deref()
        .ok_or_else(|| AppError::Message("请先在设置中选择 AI 厂商和模型".to_string()))?;
    let provider = settings
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| AppError::Message("当前 AI 厂商配置不存在".to_string()))?;
    let model = provider
        .selected_model
        .as_deref()
        .ok_or_else(|| AppError::Message("请先远程获取并选择一个模型".to_string()))?;
    let api_key = read_api_key(data_root, provider_id)?;
    let prompt = diagnostic_prompt(issue, managers, machine, guidance, scan);
    let content = match provider.protocol.as_str() {
        "anthropic" => analyze_anthropic(client, provider, model, &api_key, &prompt).await?,
        "gemini" => analyze_gemini(client, provider, model, &api_key, &prompt).await?,
        _ => analyze_openai_compatible(client, provider, model, &api_key, &prompt).await?,
    };
    Ok(AiDiagnosticAnalysis {
        provider_id: provider.id.clone(),
        provider_name: provider.display_name.clone(),
        model: model.to_string(),
        issue_code: issue.code.clone(),
        generated_at: Utc::now(),
        content,
    })
}

fn builtin_provider(id: &str) -> Option<AiProviderConfig> {
    builtin_providers()
        .into_iter()
        .find(|provider| provider.id == id)
}

fn builtin_providers() -> Vec<AiProviderConfig> {
    [
        ("openai", "OpenAI", "openai", "https://api.openai.com/v1"),
        (
            "anthropic",
            "Anthropic / Claude",
            "anthropic",
            "https://api.anthropic.com/v1",
        ),
        (
            "kimi",
            "Kimi / Moonshot",
            "openai",
            "https://api.moonshot.cn/v1",
        ),
        ("deepseek", "DeepSeek", "openai", "https://api.deepseek.com"),
        (
            "glm",
            "智谱 GLM",
            "openai",
            "https://open.bigmodel.cn/api/paas/v4",
        ),
        ("grok", "xAI / Grok", "openai", "https://api.x.ai/v1"),
        (
            "qwen",
            "阿里云百炼 / Qwen",
            "openai",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
        ),
        (
            "gemini",
            "Google Gemini",
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta",
        ),
        ("custom", "第三方兼容服务", "openai", "https://"),
    ]
    .into_iter()
    .map(|(id, display_name, protocol, base_url)| AiProviderConfig {
        id: id.to_string(),
        display_name: display_name.to_string(),
        protocol: protocol.to_string(),
        base_url: base_url.to_string(),
        selected_model: None,
        api_key_configured: false,
        builtin: true,
    })
    .collect()
}

fn merge_builtin_providers(providers: &mut Vec<AiProviderConfig>) {
    for builtin in builtin_providers() {
        if !providers.iter().any(|provider| provider.id == builtin.id) {
            providers.push(builtin);
        }
    }
    providers.sort_by_key(|provider| {
        builtin_providers()
            .iter()
            .position(|builtin| builtin.id == provider.id)
            .unwrap_or(usize::MAX)
    });
}

fn validate_provider_id(id: &str) -> AppResult<()> {
    if id.is_empty()
        || id.len() > 48
        || !id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
    {
        return Err(AppError::Message(
            "AI 厂商 ID 只能包含小写字母、数字和连字符".to_string(),
        ));
    }
    Ok(())
}

fn normalize_protocol(protocol: &str) -> AppResult<String> {
    let protocol = protocol.trim().to_ascii_lowercase();
    match protocol.as_str() {
        "openai" | "anthropic" | "gemini" => Ok(protocol),
        _ => Err(AppError::Message(
            "AI 协议必须是 openai、anthropic 或 gemini".to_string(),
        )),
    }
}

fn normalize_base_url(value: &str) -> AppResult<String> {
    let value = value.trim().trim_end_matches('/');
    let url = Url::parse(value)
        .map_err(|error| AppError::Message(format!("AI API URL 无效：{error}")))?;
    if url.scheme() != "https" {
        return Err(AppError::Message(
            "AI API URL 必须使用 HTTPS，避免密钥明文传输".to_string(),
        ));
    }
    if url.host_str().is_none() || url.username() != "" || url.password().is_some() {
        return Err(AppError::Message(
            "AI API URL 必须包含主机，且不能在 URL 中嵌入账号或密钥".to_string(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AppError::Message(
            "AI API 基础 URL 不能包含查询参数或片段".to_string(),
        ));
    }
    Ok(value.to_string())
}

async fn checked_json(request: reqwest::RequestBuilder) -> AppResult<serde_json::Value> {
    let response = request.send().await?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        let detail = detail.chars().take(500).collect::<String>();
        return Err(AppError::Message(format!(
            "AI 服务返回 HTTP {}：{}",
            status.as_u16(),
            detail
        )));
    }
    serde_json::from_slice(&bytes).map_err(AppError::from)
}

fn parse_data_models(value: &serde_json::Value) -> AppResult<Vec<AiModelInfo>> {
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AppError::Message("模型响应缺少 data 数组".to_string()))?;
    Ok(data
        .iter()
        .filter_map(|model| {
            let id = model.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            let display_name = model
                .get("display_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(id);
            Some(AiModelInfo {
                id: id.to_string(),
                display_name: display_name.to_string(),
            })
        })
        .collect())
}

fn parse_gemini_models(value: &serde_json::Value) -> AppResult<Vec<AiModelInfo>> {
    let data = value
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AppError::Message("Gemini 模型响应缺少 models 数组".to_string()))?;
    Ok(data
        .iter()
        .filter(|model| {
            model
                .get("supportedGenerationMethods")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|methods| {
                    methods
                        .iter()
                        .any(|method| method.as_str() == Some("generateContent"))
                })
        })
        .filter_map(|model| {
            let name = model.get("name")?.as_str()?.trim();
            let id = model
                .get("baseModelId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| name.trim_start_matches("models/"));
            if id.is_empty() {
                return None;
            }
            Some(AiModelInfo {
                id: id.to_string(),
                display_name: model
                    .get("displayName")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
            })
        })
        .collect())
}

fn diagnostic_prompt(
    issue: &DiagnosticIssue,
    managers: &[VersionManagerInventory],
    machine: &MachineContext,
    guidance: &DiagnosticGuidance,
    scan: &EnvironmentScan,
) -> String {
    let manager_evidence = if managers.is_empty() {
        "未检测到已知版本管理器".to_string()
    } else {
        managers
            .iter()
            .map(|manager| {
                format!(
                    "{}（工具={}，当前={}，{}）",
                    manager.display_name,
                    manager.tool_ids.join("/"),
                    manager.current_version.as_deref().unwrap_or("未知"),
                    manager.evidence
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let affected_tool = crate::diagnostics::issue_tool_id(&issue.code)
        .and_then(|tool_id| scan.tools.iter().find(|tool| tool.id == tool_id));
    let inventory = affected_tool
        .map(|tool| {
            format!(
                "{}：默认={}；全部版本={}",
                tool.display_name,
                tool.default_version
                    .as_ref()
                    .map(|version| format!("{} ({})", version.version, version.path.display()))
                    .unwrap_or_else(|| "未解析".to_string()),
                tool.installed_versions
                    .iter()
                    .map(|version| {
                        format!(
                            "{} ({}, 来源={}, managed={})",
                            version.version,
                            version.path.display(),
                            version.source,
                            version.managed
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("；")
            )
        })
        .unwrap_or_else(|| "该问题未绑定单一工具清单".to_string());
    let local_guidance = format!(
        "根因候选：{}\n机器因素：{}\n本地建议：{}\n本地允许一键修复：{}",
        guidance.root_causes.join("；"),
        guidance.machine_factors.join("；"),
        guidance.recommendations.join("；"),
        guidance.one_click_available
    );
    format!(
        "请分析以下 Windows 开发环境诊断项，并用中文输出：\n\
         1. 根因与实际影响；\n\
         2. 区分正常的版本管理器 shim/链接与真实冲突；\n\
         3. 结合这台电脑的架构、安装目录、已装版本和用户/系统变量作用域给出最小风险步骤；\n\
         4. 明确哪些本地建议可直接采用，哪些仍需人工选择；\n\
         5. 提供只读验证命令、修复后的验证和回滚方式。\n\
         不要声称已经执行修复，不要生成绕过 EnvNexus AI 差异预览、备份或确认令牌的写入命令。\n\
         不要建议自动删除未知自定义变量、版本管理器 shim 或系统级配置。\n\n\
         问题代码：{}\n标题：{}\n描述：{}\n证据：{}\n\n\
         本机上下文：平台={}，Windows 架构={}，进程架构={}，数据目录={}，用户变量={}，系统变量={}\n\
         相关工具清单：{}\n\n\
         EnvNexus AI 本地规则结论（必须作为安全下限，不得绕过）：\n{}\n\n\
         已检测版本管理器：\n{}",
        issue.code,
        issue.title,
        issue.detail,
        issue.evidence.as_deref().unwrap_or("无"),
        machine.platform,
        machine.windows_architecture,
        machine.process_architecture,
        machine.data_root.display(),
        machine.user_environment_variable_count,
        machine.system_environment_variable_count,
        inventory,
        local_guidance,
        manager_evidence
    )
}

async fn analyze_openai_compatible(
    client: &reqwest::Client,
    provider: &AiProviderConfig,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> AppResult<String> {
    let endpoint = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let value = checked_json(
        client
            .post(endpoint)
            .bearer_auth(api_key)
            .json(&serde_json::json!({
                "model": model,
                "messages": [
                    {
                        "role": "system",
                        "content": "你是谨慎的 Windows 开发环境诊断助手。只提供基于证据的分析和可回滚建议。"
                    },
                    { "role": "user", "content": prompt }
                ],
                "temperature": 0.1
            })),
    )
    .await?;
    value
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::Message("AI 响应缺少 choices[0].message.content".to_string()))
}

async fn analyze_anthropic(
    client: &reqwest::Client,
    provider: &AiProviderConfig,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> AppResult<String> {
    let endpoint = format!("{}/messages", provider.base_url.trim_end_matches('/'));
    let value = checked_json(
        client
            .post(endpoint)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&serde_json::json!({
                "model": model,
                "max_tokens": 1800,
                "temperature": 0.1,
                "system": "你是谨慎的 Windows 开发环境诊断助手。只提供基于证据的分析和可回滚建议。",
                "messages": [{ "role": "user", "content": prompt }]
            })),
    )
    .await?;
    value
        .get("content")
        .and_then(serde_json::Value::as_array)
        .and_then(|content| {
            content
                .iter()
                .find(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        })
        .and_then(|block| block.get("text"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::Message("Claude 响应缺少文本内容".to_string()))
}

async fn analyze_gemini(
    client: &reqwest::Client,
    provider: &AiProviderConfig,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> AppResult<String> {
    let model = model.trim_start_matches("models/");
    let endpoint = format!(
        "{}/models/{}:generateContent",
        provider.base_url.trim_end_matches('/'),
        model
    );
    let value = checked_json(
        client
            .post(endpoint)
            .header("x-goog-api-key", api_key)
            .json(&serde_json::json!({
                "systemInstruction": {
                    "parts": [{ "text": "你是谨慎的 Windows 开发环境诊断助手。只提供基于证据的分析和可回滚建议。" }]
                },
                "contents": [{
                    "role": "user",
                    "parts": [{ "text": prompt }]
                }],
                "generationConfig": { "temperature": 0.1, "maxOutputTokens": 1800 }
            })),
    )
    .await?;
    value
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::Message("Gemini 响应缺少候选文本".to_string()))
}

fn settings_path(data_root: &Path) -> PathBuf {
    data_root.join("config").join("ai-providers.json")
}

fn secrets_path(data_root: &Path) -> PathBuf {
    data_root.join("config").join("ai-secrets.dpapi.json")
}

fn ai_config_root(data_root: &Path) -> PathBuf {
    data_root.join("config").join("ai")
}

fn providers_directory(data_root: &Path) -> PathBuf {
    ai_config_root(data_root).join("providers")
}

fn secrets_directory(data_root: &Path) -> PathBuf {
    ai_config_root(data_root).join("secrets")
}

fn provider_path(data_root: &Path, provider_id: &str) -> PathBuf {
    providers_directory(data_root).join(format!("{provider_id}.json"))
}

fn provider_secret_path(data_root: &Path, provider_id: &str) -> PathBuf {
    secrets_directory(data_root).join(format!("{provider_id}.dpapi.json"))
}

fn active_provider_path(data_root: &Path) -> PathBuf {
    ai_config_root(data_root).join("active-provider.json")
}

fn write_provider_document(data_root: &Path, provider: &AiProviderConfig) -> AppResult<()> {
    validate_provider_id(&provider.id)?;
    let mut provider = provider.clone();
    provider.api_key_configured = false;
    write_json_atomic(
        &provider_path(data_root, &provider.id),
        &ProviderDocument {
            schema_version: SETTINGS_SCHEMA,
            provider,
        },
    )
}

fn read_provider_documents(data_root: &Path) -> AppResult<Vec<AiProviderConfig>> {
    let directory = providers_directory(data_root);
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut providers = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let document = serde_json::from_slice::<ProviderDocument>(&fs::read(&path)?)?;
        if document.schema_version != SETTINGS_SCHEMA {
            return Err(AppError::Message(format!(
                "不支持的 AI 厂商配置版本 {}：{}",
                document.schema_version,
                path.display()
            )));
        }
        validate_provider_id(&document.provider.id)?;
        let expected_path = provider_path(data_root, &document.provider.id);
        if path != expected_path {
            return Err(AppError::Message(format!(
                "AI 厂商配置文件名与厂商 ID 不一致：{}",
                path.display()
            )));
        }
        providers.push(document.provider);
    }
    Ok(providers)
}

fn read_active_provider(data_root: &Path) -> AppResult<ActiveProviderDocument> {
    let path = active_provider_path(data_root);
    let bytes = fs::read(path)?;
    let document = serde_json::from_slice::<ActiveProviderDocument>(&bytes)?;
    if document.schema_version != SETTINGS_SCHEMA {
        return Err(AppError::Message(
            "不支持的当前 AI 厂商配置版本".to_string(),
        ));
    }
    if let Some(provider_id) = document.active_provider_id.as_deref() {
        validate_provider_id(provider_id)?;
    }
    Ok(document)
}

fn migrate_legacy_storage(data_root: &Path) -> AppResult<()> {
    if active_provider_path(data_root).is_file() {
        return Ok(());
    }
    let legacy_settings_path = settings_path(data_root);
    let legacy_settings = if legacy_settings_path.is_file() {
        let settings = serde_json::from_slice::<AiSettings>(&fs::read(&legacy_settings_path)?)?;
        if settings.schema_version != SETTINGS_SCHEMA {
            return Err(AppError::Message(format!(
                "不支持的 AI 设置版本 {}",
                settings.schema_version
            )));
        }
        settings
    } else {
        AiSettings {
            schema_version: SETTINGS_SCHEMA,
            active_provider_id: None,
            providers: Vec::new(),
        }
    };
    for provider in &legacy_settings.providers {
        write_provider_document(data_root, provider)?;
    }

    let legacy_secrets = read_legacy_secrets(data_root)?;
    for (provider_id, encrypted_key) in legacy_secrets.encrypted_keys {
        validate_provider_id(&provider_id)?;
        write_json_atomic(
            &provider_secret_path(data_root, &provider_id),
            &ProviderSecretDocument {
                schema_version: SETTINGS_SCHEMA,
                encrypted_key,
            },
        )?;
    }
    write_json_atomic(
        &active_provider_path(data_root),
        &ActiveProviderDocument {
            schema_version: SETTINGS_SCHEMA,
            active_provider_id: legacy_settings.active_provider_id,
        },
    )
}

fn read_legacy_secrets(data_root: &Path) -> AppResult<LegacySecretStore> {
    let path = secrets_path(data_root);
    if !path.is_file() {
        return Ok(LegacySecretStore::default());
    }
    let bytes = fs::read(path)?;
    let secrets = serde_json::from_slice::<LegacySecretStore>(&bytes)?;
    if secrets.schema_version != SETTINGS_SCHEMA {
        return Err(AppError::Message("不支持的 AI 密钥存储版本".to_string()));
    }
    Ok(secrets)
}

fn read_api_key(data_root: &Path, provider_id: &str) -> AppResult<String> {
    validate_provider_id(provider_id)?;
    migrate_legacy_storage(data_root)?;
    let path = provider_secret_path(data_root, provider_id);
    if !path.is_file() {
        return Err(AppError::Message("该 AI 厂商尚未保存 API Key".to_string()));
    }
    let document = serde_json::from_slice::<ProviderSecretDocument>(&fs::read(path)?)?;
    if document.schema_version != SETTINGS_SCHEMA {
        return Err(AppError::Message("不支持的 AI 密钥存储版本".to_string()));
    }
    let bytes = unprotect_secret(&document.encrypted_key)?;
    String::from_utf8(bytes)
        .map_err(|_| AppError::Message("解密后的 API Key 不是有效文本".to_string()))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)?;
    if path.exists() {
        let previous = path.with_extension("json.previous");
        if previous.exists() {
            fs::remove_file(&previous)?;
        }
        fs::rename(path, &previous)?;
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::rename(previous, path);
            return Err(error.into());
        }
        let _ = fs::remove_file(previous);
    } else {
        fs::rename(temporary, path)?;
    }
    Ok(())
}

#[cfg(windows)]
fn protect_secret(bytes: &[u8]) -> AppResult<String> {
    use std::{ptr, slice};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData},
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes
            .len()
            .try_into()
            .map_err(|_| AppError::Message("API Key 过长".to_string()))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let encrypted = unsafe { slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let encoded = hex::encode(encrypted);
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(encoded)
}

#[cfg(windows)]
fn unprotect_secret(encoded: &str) -> AppResult<Vec<u8>> {
    use std::{ptr, slice};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };

    let mut encrypted = hex::decode(encoded)
        .map_err(|error| AppError::Message(format!("AI 密钥数据损坏：{error}")))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted
            .len()
            .try_into()
            .map_err(|_| AppError::Message("加密 API Key 数据过长".to_string()))?,
        pbData: encrypted.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let decrypted =
        unsafe { slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(decrypted)
}

#[cfg(not(windows))]
fn protect_secret(_bytes: &[u8]) -> AppResult<String> {
    Err(AppError::Message(
        "当前平台不支持 Windows DPAPI 密钥存储".to_string(),
    ))
}

#[cfg(not(windows))]
fn unprotect_secret(_encoded: &str) -> AppResult<Vec<u8>> {
    Err(AppError::Message(
        "当前平台不支持 Windows DPAPI 密钥存储".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_compatible_model_list() {
        let value = serde_json::json!({
            "data": [
                { "id": "model-b", "object": "model" },
                { "id": "model-a", "display_name": "Model A" }
            ]
        });
        let models = parse_data_models(&value).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[1].display_name, "Model A");
    }

    #[test]
    fn parses_only_gemini_generate_content_models() {
        let value = serde_json::json!({
            "models": [
                {
                    "name": "models/gemini-a",
                    "baseModelId": "gemini-a",
                    "displayName": "Gemini A",
                    "supportedGenerationMethods": ["generateContent"]
                },
                {
                    "name": "models/embed-a",
                    "supportedGenerationMethods": ["embedContent"]
                }
            ]
        });
        let models = parse_gemini_models(&value).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-a");
    }

    #[test]
    fn rejects_non_https_ai_base_url() {
        assert!(normalize_base_url("http://example.com/v1").is_err());
        assert!(normalize_base_url("https://example.com/v1").is_ok());
    }

    #[test]
    fn merges_all_builtin_providers_without_overwriting_saved_values() {
        let mut providers = vec![AiProviderConfig {
            id: "openai".to_string(),
            display_name: "自定义 OpenAI".to_string(),
            protocol: "openai".to_string(),
            base_url: "https://gateway.example.com/v1".to_string(),
            selected_model: Some("model".to_string()),
            api_key_configured: false,
            builtin: true,
        }];
        merge_builtin_providers(&mut providers);
        assert_eq!(providers.len(), 9);
        assert_eq!(providers[0].base_url, "https://gateway.example.com/v1");
    }

    #[test]
    fn saves_each_provider_in_an_independent_document() {
        let temporary = tempfile::tempdir().unwrap();
        save_provider(
            temporary.path(),
            AiProviderInput {
                id: "openai".to_string(),
                display_name: "OpenAI Gateway".to_string(),
                protocol: "openai".to_string(),
                base_url: "https://openai.example.com/v1".to_string(),
                selected_model: Some("openai-model".to_string()),
                api_key: None,
            },
        )
        .unwrap();
        let openai_path = provider_path(temporary.path(), "openai");
        let openai_before = fs::read(&openai_path).unwrap();

        let settings = save_provider(
            temporary.path(),
            AiProviderInput {
                id: "deepseek".to_string(),
                display_name: "DeepSeek Gateway".to_string(),
                protocol: "openai".to_string(),
                base_url: "https://deepseek.example.com".to_string(),
                selected_model: Some("deepseek-model".to_string()),
                api_key: None,
            },
        )
        .unwrap();

        assert_eq!(fs::read(openai_path).unwrap(), openai_before);
        assert!(provider_path(temporary.path(), "deepseek").is_file());
        assert_eq!(settings.active_provider_id, None);
        assert_eq!(
            settings
                .providers
                .iter()
                .find(|provider| provider.id == "openai")
                .unwrap()
                .base_url,
            "https://openai.example.com/v1"
        );
        assert_eq!(
            settings
                .providers
                .iter()
                .find(|provider| provider.id == "deepseek")
                .unwrap()
                .base_url,
            "https://deepseek.example.com"
        );
    }

    #[test]
    fn migrates_the_legacy_aggregate_provider_file_without_losing_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let legacy = AiSettings {
            schema_version: SETTINGS_SCHEMA,
            active_provider_id: Some("openai".to_string()),
            providers: vec![AiProviderConfig {
                id: "openai".to_string(),
                display_name: "Migrated OpenAI".to_string(),
                protocol: "openai".to_string(),
                base_url: "https://migration.example.com/v1".to_string(),
                selected_model: Some("migrated-model".to_string()),
                api_key_configured: false,
                builtin: true,
            }],
        };
        write_json_atomic(&settings_path(temporary.path()), &legacy).unwrap();

        let settings = read_settings(temporary.path()).unwrap();
        assert_eq!(settings.active_provider_id.as_deref(), Some("openai"));
        assert!(provider_path(temporary.path(), "openai").is_file());
        assert!(active_provider_path(temporary.path()).is_file());
        let provider = settings
            .providers
            .iter()
            .find(|provider| provider.id == "openai")
            .unwrap();
        assert_eq!(provider.display_name, "Migrated OpenAI");
        assert_eq!(provider.selected_model.as_deref(), Some("migrated-model"));
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_secret_round_trip_uses_current_windows_user() {
        let secret = b"envpilot-test-key-not-a-real-credential";
        let encrypted = protect_secret(secret).unwrap();
        assert_ne!(encrypted, hex::encode(secret));
        assert_eq!(unprotect_secret(&encrypted).unwrap(), secret);
    }

    #[cfg(windows)]
    #[test]
    fn provider_keys_are_independent_and_activation_is_explicit() {
        let temporary = tempfile::tempdir().unwrap();
        for (id, url, key) in [
            ("openai", "https://openai.example.com/v1", "openai-test-key"),
            (
                "deepseek",
                "https://deepseek.example.com",
                "deepseek-test-key",
            ),
        ] {
            save_provider(
                temporary.path(),
                AiProviderInput {
                    id: id.to_string(),
                    display_name: id.to_string(),
                    protocol: "openai".to_string(),
                    base_url: url.to_string(),
                    selected_model: Some(format!("{id}-model")),
                    api_key: Some(key.to_string()),
                },
            )
            .unwrap();
        }

        assert_eq!(
            read_api_key(temporary.path(), "openai").unwrap(),
            "openai-test-key"
        );
        assert_eq!(
            read_api_key(temporary.path(), "deepseek").unwrap(),
            "deepseek-test-key"
        );
        assert_eq!(
            read_settings(temporary.path()).unwrap().active_provider_id,
            None
        );

        let active = activate_provider(temporary.path(), "deepseek").unwrap();
        assert_eq!(active.active_provider_id.as_deref(), Some("deepseek"));
        clear_api_key(temporary.path(), "openai").unwrap();
        assert!(read_api_key(temporary.path(), "openai").is_err());
        assert_eq!(
            read_api_key(temporary.path(), "deepseek").unwrap(),
            "deepseek-test-key"
        );
    }
}
