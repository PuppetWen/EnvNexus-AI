use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    environment::{
        EnvironmentMap, environment_fingerprint, get_case_insensitive, read_environment, split_path,
    },
    error::{AppError, AppResult},
    model::{
        DiagnosticIssue, EnvironmentBackupSummary, EnvironmentDiff, EnvironmentScope,
        InstallRequest, IssueLevel, OperationPlan, PlanStep, PlannedAction, RemoteVersion,
    },
    plugins::{PluginRegistry, ToolDescriptor},
};

#[cfg(windows)]
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_SET_VALUE},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentBackup {
    schema_version: u32,
    created_at: chrono::DateTime<Utc>,
    operation_id: String,
    user_environment: EnvironmentMap,
}

pub struct PlanService {
    plans: Mutex<HashMap<String, OperationPlan>>,
    data_root: PathBuf,
}

impl PlanService {
    pub fn new(data_root: PathBuf) -> Self {
        Self {
            plans: Mutex::new(HashMap::new()),
            data_root,
        }
    }

    pub fn preview_switch(
        &self,
        registry: &PluginRegistry,
        tool_id: &str,
        installation_path: PathBuf,
    ) -> AppResult<OperationPlan> {
        let plugin = registry.get(tool_id)?;
        let descriptor = plugin.descriptor();
        let installation_path = crate::paths::canonicalize_simplified(&installation_path)
            .map_err(|error| {
                AppError::Message(format!(
                    "所选安装目录无法访问（{}）：{error}",
                    installation_path.display()
                ))
            })?;
        let activation = activation_for(descriptor, &installation_path)?;
        let user = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        let current_fingerprint = combined_fingerprint(&user, &system);
        let (diffs, added, removed) = build_environment_diffs(descriptor, &activation, &user);
        let conflicts = system_path_conflicts(descriptor, &installation_path, &system);
        let requires_elevation = !conflicts.is_empty();
        let created_at = Utc::now();
        let id = Uuid::new_v4().to_string();
        let token = confirmation_token(&id, &current_fingerprint);
        let mut warnings =
            vec!["环境变量只影响新启动的终端和应用，已打开的进程不会被改写。".to_string()];
        if requires_elevation {
            warnings.push(
                "系统 PATH 中的同名命令会先于用户 PATH 命中；当前安全模式将阻止应用此计划。"
                    .to_string(),
            );
        }
        let mut steps = vec![
            PlanStep {
                kind: "backup".to_string(),
                description: "备份当前用户级环境变量".to_string(),
                destructive: false,
            },
            PlanStep {
                kind: "environment".to_string(),
                description: format!(
                    "更新用户 PATH（新增 {} 项，移除 {} 项）",
                    added.len(),
                    removed.len()
                ),
                destructive: !removed.is_empty(),
            },
        ];
        for name in activation.variables.keys() {
            steps.push(PlanStep {
                kind: "environment".to_string(),
                description: format!("设置用户变量 {name}"),
                destructive: false,
            });
        }
        steps.push(PlanStep {
            kind: "broadcast".to_string(),
            description: "广播 Windows 环境已变更并回读验证".to_string(),
            destructive: false,
        });

        let plan = OperationPlan {
            id: id.clone(),
            tool_id: tool_id.to_string(),
            title: format!("切换默认 {}", descriptor.display_name),
            summary: format!(
                "将 {} 的默认安装切换到 {}",
                descriptor.display_name,
                installation_path.display()
            ),
            created_at,
            expires_at: created_at + Duration::minutes(10),
            confirmation_token: token,
            requires_elevation,
            warnings,
            conflicts,
            environment_diffs: diffs,
            steps,
            environment_fingerprint: current_fingerprint,
            action: PlannedAction::Switch {
                tool_id: tool_id.to_string(),
                installation_path,
            },
        };
        self.plans
            .lock()
            .map_err(|_| AppError::Message("计划存储锁已损坏".to_string()))?
            .insert(id, plan.clone());
        Ok(plan)
    }

    pub fn preview_install(
        &self,
        descriptor: &ToolDescriptor,
        remote: &RemoteVersion,
        root: PathBuf,
    ) -> AppResult<OperationPlan> {
        let root = crate::paths::canonicalize_simplified(&root).map_err(|error| {
            AppError::Message(format!("安装根目录无法访问（{}）：{error}", root.display()))
        })?;
        if !root.is_dir() || root.parent().is_none() {
            return Err(AppError::UnsafePath(root));
        }
        let destination = installation_destination(descriptor.id, &remote.version, &root)?;
        if destination.exists() {
            return Err(AppError::Message(format!(
                "目标版本目录已存在，请改用修复或卸载：{}",
                destination.display()
            )));
        }
        let download_url = remote.download_url.clone().ok_or_else(|| {
            AppError::InvalidSource(format!(
                "{} {} 没有 Windows 下载文件",
                descriptor.id, remote.version
            ))
        })?;
        validate_download_url(descriptor.id, &download_url)?;
        let user = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        let fingerprint = combined_fingerprint(&user, &system);
        let created_at = Utc::now();
        let id = Uuid::new_v4().to_string();
        let token = confirmation_token(&id, &fingerprint);
        let mut warnings = Vec::new();
        if remote.checksum.is_none() || remote.checksum_algorithm.is_none() {
            warnings.push(
                "该官方源没有在 API 中提供可用校验值；执行前应单独评估供应链风险。".to_string(),
            );
        }
        if root
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("c:\\")
        {
            warnings
                .push("你选择了 C 盘；EnvNexus AI 不会阻止，但会在执行前明确提示。".to_string());
        }
        if descriptor.id.starts_with("android") || descriptor.id == "adb" {
            warnings
                .push("Android 组件将安装在同一 Android 根目录下，不迁移已有 SDK。".to_string());
        }
        let plan = OperationPlan {
            id: id.clone(),
            tool_id: descriptor.id.to_string(),
            title: format!("安装 {} {}", descriptor.display_name, remote.version),
            summary: format!("下载、校验并安装到 {}", destination.display()),
            created_at,
            expires_at: created_at + Duration::minutes(20),
            confirmation_token: token,
            requires_elevation: false,
            warnings,
            conflicts: Vec::new(),
            environment_diffs: Vec::new(),
            steps: vec![
                PlanStep {
                    kind: "download".to_string(),
                    description: format!("从 {} 断点续传发行包", source_host(&download_url)),
                    destructive: false,
                },
                PlanStep {
                    kind: "verify".to_string(),
                    description: remote
                        .checksum_algorithm
                        .as_ref()
                        .map(|algorithm| format!("验证 {} 校验值", algorithm.to_ascii_uppercase()))
                        .unwrap_or_else(|| "记录未提供校验值的供应链警告".to_string()),
                    destructive: false,
                },
                PlanStep {
                    kind: "extract".to_string(),
                    description: format!(
                        "安全解压到暂存目录，再原子提交到 {}",
                        destination.display()
                    ),
                    destructive: false,
                },
                PlanStep {
                    kind: "verify".to_string(),
                    description: "运行版本命令并写入受管安装清单".to_string(),
                    destructive: false,
                },
            ],
            environment_fingerprint: fingerprint,
            action: PlannedAction::Install(InstallRequest {
                tool_id: descriptor.id.to_string(),
                version: remote.version.clone(),
                root,
                destination,
                download_url,
                checksum_algorithm: remote.checksum_algorithm.clone(),
                checksum: remote.checksum.clone(),
            }),
        };
        self.plans
            .lock()
            .map_err(|_| AppError::Message("计划存储锁已损坏".to_string()))?
            .insert(id, plan.clone());
        Ok(plan)
    }

    pub fn preview_uninstall(
        &self,
        registry: &PluginRegistry,
        tool_id: &str,
        installation_path: PathBuf,
    ) -> AppResult<OperationPlan> {
        let descriptor = registry.get(tool_id)?.descriptor().clone();
        let installation_path = crate::paths::canonicalize_simplified(&installation_path)
            .map_err(|error| {
                AppError::Message(format!(
                    "安装目录无法访问（{}）：{error}",
                    installation_path.display()
                ))
            })?;
        if installation_path.parent().is_none()
            || !installation_path.join(".envpilot-install.json").is_file()
        {
            return Err(AppError::UnsafePath(installation_path));
        }
        let user = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        let user_environment_after =
            environment_after_uninstall(&descriptor, &installation_path, &user);
        let environment_diffs = compare_environment_maps(&user, &user_environment_after);
        let fingerprint = combined_fingerprint(&user, &system);
        let created_at = Utc::now();
        let id = Uuid::new_v4().to_string();
        let token = confirmation_token(&id, &fingerprint);
        let plan = OperationPlan {
            id: id.clone(),
            tool_id: tool_id.to_string(),
            title: format!("卸载 {}", descriptor.display_name),
            summary: format!("删除 EnvNexus AI 受管目录 {}", installation_path.display()),
            created_at,
            expires_at: created_at + Duration::minutes(10),
            confirmation_token: token,
            requires_elevation: false,
            warnings: {
                let mut warnings = vec![
                    "仅删除带有有效 EnvNexus AI 安装清单的目录；外部安装不会被递归删除。"
                        .to_string(),
                ];
                if !environment_diffs.is_empty() {
                    warnings.push(
                        "该版本仍被用户环境引用；卸载前将备份并清理对应的用户 PATH/工具变量。"
                            .to_string(),
                    );
                }
                warnings
            },
            conflicts: Vec::new(),
            environment_diffs,
            steps: {
                let mut steps = Vec::new();
                if user_environment_after != user {
                    steps.push(PlanStep {
                        kind: "backup".to_string(),
                        description: "备份当前用户环境并清理指向该版本的引用".to_string(),
                        destructive: false,
                    });
                }
                steps.extend([
                    PlanStep {
                        kind: "verify".to_string(),
                        description: "验证安装清单与受管根目录边界".to_string(),
                        destructive: false,
                    },
                    PlanStep {
                        kind: "uninstall".to_string(),
                        description: "先重命名隔离，再删除该版本目录".to_string(),
                        destructive: true,
                    },
                ]);
                steps
            },
            environment_fingerprint: fingerprint,
            action: PlannedAction::Uninstall {
                tool_id: tool_id.to_string(),
                installation_path,
                user_environment_after: (user_environment_after != user)
                    .then_some(user_environment_after),
            },
        };
        self.plans
            .lock()
            .map_err(|_| AppError::Message("计划存储锁已损坏".to_string()))?
            .insert(id, plan.clone());
        Ok(plan)
    }

    pub fn preview_repair(
        &self,
        descriptor: &ToolDescriptor,
        remote: &RemoteVersion,
        root: PathBuf,
        destination: PathBuf,
    ) -> AppResult<OperationPlan> {
        let root = crate::paths::canonicalize_simplified(&root)?;
        let destination = crate::paths::canonicalize_simplified(&destination)?;
        if !destination.starts_with(&root)
            || destination == root
            || !destination.join(".envpilot-install.json").is_file()
        {
            return Err(AppError::UnsafePath(destination));
        }
        let download_url = remote.download_url.clone().ok_or_else(|| {
            AppError::InvalidSource(format!(
                "{} {} 没有 Windows 下载文件",
                descriptor.id, remote.version
            ))
        })?;
        validate_download_url(descriptor.id, &download_url)?;
        let user = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        let fingerprint = combined_fingerprint(&user, &system);
        let created_at = Utc::now();
        let id = Uuid::new_v4().to_string();
        let token = confirmation_token(&id, &fingerprint);
        let mut warnings =
            vec!["修复会先在同一磁盘创建新版本，验证通过后再替换现有受管目录。".to_string()];
        if remote.checksum.is_none() {
            warnings.push("官方清单未提供校验值，修复计划保留供应链警告。".to_string());
        }
        let plan = OperationPlan {
            id: id.clone(),
            tool_id: descriptor.id.to_string(),
            title: format!("修复 {} {}", descriptor.display_name, remote.version),
            summary: format!("重新下载并验证 {}", destination.display()),
            created_at,
            expires_at: created_at + Duration::minutes(20),
            confirmation_token: token,
            requires_elevation: false,
            warnings,
            conflicts: Vec::new(),
            environment_diffs: Vec::new(),
            steps: vec![
                PlanStep {
                    kind: "download".to_string(),
                    description: "断点续传并校验官方发行包".to_string(),
                    destructive: false,
                },
                PlanStep {
                    kind: "extract".to_string(),
                    description: "在同一受管根目录创建修复暂存版本".to_string(),
                    destructive: false,
                },
                PlanStep {
                    kind: "repair".to_string(),
                    description: "隔离旧目录、提交新目录并运行版本验证".to_string(),
                    destructive: true,
                },
                PlanStep {
                    kind: "rollback".to_string(),
                    description: "验证失败时自动恢复旧目录".to_string(),
                    destructive: false,
                },
            ],
            environment_fingerprint: fingerprint,
            action: PlannedAction::Repair(InstallRequest {
                tool_id: descriptor.id.to_string(),
                version: remote.version.clone(),
                root,
                destination,
                download_url,
                checksum_algorithm: remote.checksum_algorithm.clone(),
                checksum: remote.checksum.clone(),
            }),
        };
        self.plans
            .lock()
            .map_err(|_| AppError::Message("计划存储锁已损坏".to_string()))?
            .insert(id, plan.clone());
        Ok(plan)
    }

    pub fn list_backups(&self) -> AppResult<Vec<EnvironmentBackupSummary>> {
        let directory = self.data_root.join("backups").join("environment");
        let mut backups = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_file()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Ok(backup) = serde_json::from_slice::<EnvironmentBackup>(&bytes) else {
                continue;
            };
            backups.push(EnvironmentBackupSummary {
                id: entry.file_name().to_string_lossy().into_owned(),
                created_at: backup.created_at,
                operation_id: backup.operation_id,
                variable_count: backup.user_environment.len(),
            });
        }
        backups.sort_by_key(|backup| std::cmp::Reverse(backup.created_at));
        Ok(backups)
    }

    pub fn preview_restore(&self, backup_id: &str) -> AppResult<OperationPlan> {
        if backup_id.is_empty() || backup_id.contains(['/', '\\']) || !backup_id.ends_with(".json")
        {
            return Err(AppError::UnsafePath(PathBuf::from(backup_id)));
        }
        let directory = fs::canonicalize(self.data_root.join("backups").join("environment"))?;
        let backup_path = fs::canonicalize(directory.join(backup_id))?;
        if backup_path.parent() != Some(directory.as_path()) {
            return Err(AppError::UnsafePath(backup_path));
        }
        let backup = serde_json::from_slice::<EnvironmentBackup>(&fs::read(&backup_path)?)?;
        if backup.schema_version != 1 {
            return Err(AppError::Message("不支持的环境备份版本".to_string()));
        }
        let user = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        let fingerprint = combined_fingerprint(&user, &system);
        let created_at = Utc::now();
        let id = Uuid::new_v4().to_string();
        let token = confirmation_token(&id, &fingerprint);
        let diffs = compare_environment_maps(&user, &backup.user_environment);
        let plan = OperationPlan {
            id: id.clone(),
            tool_id: "environment".to_string(),
            title: "恢复用户环境备份".to_string(),
            summary: format!(
                "恢复 {} 创建的备份（{} 个变量）",
                backup.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
                backup.user_environment.len()
            ),
            created_at,
            expires_at: created_at + Duration::minutes(10),
            confirmation_token: token,
            requires_elevation: false,
            warnings: vec![
                "恢复只覆盖 HKCU 用户级环境；系统级变量不会改动。".to_string(),
                "执行前会再次备份当前用户环境，因此恢复操作本身也可以撤销。".to_string(),
            ],
            conflicts: Vec::new(),
            environment_diffs: diffs,
            steps: vec![
                PlanStep {
                    kind: "backup".to_string(),
                    description: "备份当前用户环境".to_string(),
                    destructive: false,
                },
                PlanStep {
                    kind: "restore".to_string(),
                    description: "恢复备份中的用户级变量并删除备份中不存在的用户变量".to_string(),
                    destructive: true,
                },
                PlanStep {
                    kind: "verify".to_string(),
                    description: "回读注册表、验证快照并广播环境变化".to_string(),
                    destructive: false,
                },
            ],
            environment_fingerprint: fingerprint,
            action: PlannedAction::RestoreEnvironment { backup_path },
        };
        self.plans
            .lock()
            .map_err(|_| AppError::Message("计划存储锁已损坏".to_string()))?
            .insert(id, plan.clone());
        Ok(plan)
    }

    pub fn preview_diagnostic_repair(&self, issue_code: &str) -> AppResult<OperationPlan> {
        let user = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        let (updated, summary, mut warnings) =
            diagnostic_environment_update(issue_code, &user, &system)?;
        if updated == user {
            return Err(AppError::Message(
                "当前用户环境已不再包含该问题，请手动重新扫描".to_string(),
            ));
        }
        let environment_diffs = compare_environment_maps(&user, &updated);
        let fingerprint = combined_fingerprint(&user, &system);
        let created_at = Utc::now();
        let id = Uuid::new_v4().to_string();
        let token = confirmation_token(&id, &fingerprint);
        warnings.extend([
            "只修改 HKCU 用户级环境；系统 PATH 和系统变量不会被写入。".to_string(),
            "已识别的 pyenv、NVM、fnm、Volta、rustup、Jabba、goenv 根目录会被保护。".to_string(),
            "执行前自动备份完整用户环境，失败时回滚，之后可在变更中心一键恢复。".to_string(),
        ]);
        let plan = OperationPlan {
            id: id.clone(),
            tool_id: "environment".to_string(),
            title: "修复用户环境诊断项".to_string(),
            summary: summary.clone(),
            created_at,
            expires_at: created_at + Duration::minutes(10),
            confirmation_token: token,
            requires_elevation: false,
            warnings,
            conflicts: Vec::new(),
            environment_diffs,
            steps: vec![
                PlanStep {
                    kind: "backup".to_string(),
                    description: "备份完整 HKCU 用户环境".to_string(),
                    destructive: false,
                },
                PlanStep {
                    kind: "diagnostic-repair".to_string(),
                    description: summary.clone(),
                    destructive: true,
                },
                PlanStep {
                    kind: "verify".to_string(),
                    description: "回读注册表、核对环境指纹并广播环境变化".to_string(),
                    destructive: false,
                },
            ],
            environment_fingerprint: fingerprint,
            action: PlannedAction::UpdateUserEnvironment {
                updated,
                reason: summary,
            },
        };
        self.plans
            .lock()
            .map_err(|_| AppError::Message("计划存储锁已损坏".to_string()))?
            .insert(id, plan.clone());
        Ok(plan)
    }

    pub fn preview_command_directory(
        &self,
        command_directory: PathBuf,
        enable: bool,
    ) -> AppResult<OperationPlan> {
        let command_directory = crate::paths::canonicalize_simplified(&command_directory)
            .map_err(|error| {
                AppError::Message(format!(
                    "命令目录无法访问（{}）：{error}",
                    command_directory.display()
                ))
            })?;
        let user = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        let updated = environment_with_command_directory(&user, &command_directory, enable);
        if updated == user {
            return Err(AppError::Message(if enable {
                "EnvNexus AI 命令目录已存在于用户 PATH".to_string()
            } else {
                "EnvNexus AI 命令目录当前不在用户 PATH".to_string()
            }));
        }
        let environment_diffs = compare_environment_maps(&user, &updated);
        let conflicts = if enable {
            command_script_conflicts(&command_directory, &user, &system)
        } else {
            Vec::new()
        };
        let requires_elevation = conflicts
            .iter()
            .any(|conflict| conflict.level == IssueLevel::Error);
        let fingerprint = combined_fingerprint(&user, &system);
        let created_at = Utc::now();
        let id = Uuid::new_v4().to_string();
        let token = confirmation_token(&id, &fingerprint);
        let action_text = if enable { "启用" } else { "停用" };
        let summary = format!(
            "{action_text}任意 CMD/PowerShell 中的 EnvNexus AI 工具命令：{}",
            command_directory.display()
        );
        let mut warnings = vec![
            "只修改 HKCU 用户 PATH，不修改系统 PATH 或 HKLM。".to_string(),
            "变更只对之后打开的 CMD、PowerShell 和应用生效。".to_string(),
            if enable {
                "命令脚本调用同一个 EnvNexus-AI.exe 主程序。".to_string()
            } else {
                "停用只从 PATH 移除命令目录，不删除工具链、配置或操作日志。".to_string()
            },
        ];
        if requires_elevation {
            warnings.push(
                "系统 PATH 中存在同名命令；仅添加用户 PATH 不能保证 EnvNexus AI 命令优先生效，安全模式将阻止执行。"
                    .to_string(),
            );
        }
        let plan = OperationPlan {
            id: id.clone(),
            tool_id: "terminal-commands".to_string(),
            title: format!("{action_text} EnvNexus AI 工具命令"),
            summary: summary.clone(),
            created_at,
            expires_at: created_at + Duration::minutes(10),
            confirmation_token: token,
            requires_elevation,
            warnings,
            conflicts,
            environment_diffs,
            steps: vec![
                PlanStep {
                    kind: "backup".to_string(),
                    description: "备份完整 HKCU 用户环境".to_string(),
                    destructive: false,
                },
                PlanStep {
                    kind: "terminal-commands".to_string(),
                    description: format!("{action_text}用户 PATH 中的 EnvNexus AI 命令目录"),
                    destructive: !enable,
                },
                PlanStep {
                    kind: "verify".to_string(),
                    description: "回读注册表、验证环境指纹并广播环境变化".to_string(),
                    destructive: false,
                },
            ],
            environment_fingerprint: fingerprint,
            action: PlannedAction::UpdateUserEnvironment {
                updated,
                reason: summary,
            },
        };
        self.plans
            .lock()
            .map_err(|_| AppError::Message("计划存储锁已损坏".to_string()))?
            .insert(id, plan.clone());
        Ok(plan)
    }

    pub fn take_confirmed(&self, id: &str, token: &str) -> AppResult<OperationPlan> {
        let plan = {
            let mut plans = self
                .plans
                .lock()
                .map_err(|_| AppError::Message("计划存储锁已损坏".to_string()))?;
            let plan = plans.remove(id).ok_or(AppError::InvalidPlan)?;
            if plan.confirmation_token != token {
                return Err(AppError::ConfirmationMismatch);
            }
            if Utc::now() > plan.expires_at {
                return Err(AppError::InvalidPlan);
            }
            plan
        };
        if plan.requires_elevation {
            return Err(AppError::SystemScopeDenied);
        }
        let user = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        if combined_fingerprint(&user, &system) != plan.environment_fingerprint {
            return Err(AppError::StaleEnvironment);
        }
        Ok(plan)
    }

    pub fn apply_environment_plan(
        &self,
        registry: &PluginRegistry,
        plan: &OperationPlan,
    ) -> AppResult<()> {
        let user = read_environment(EnvironmentScope::User)?;
        // take_confirmed 之后仍可能有外部进程（setx、安装器）改写 HKCU；
        // 写入前用同一份快照再核对一次指纹，尽量缩小覆盖外部修改的窗口。
        let system = read_environment(EnvironmentScope::System)?;
        if combined_fingerprint(&user, &system) != plan.environment_fingerprint {
            return Err(AppError::StaleEnvironment);
        }
        match &plan.action {
            PlannedAction::Switch {
                tool_id,
                installation_path,
            } => {
                let descriptor = registry.get(tool_id)?.descriptor().clone();
                let activation = activation_for(&descriptor, installation_path)?;
                let (_, _, _) = build_environment_diffs(&descriptor, &activation, &user);
                let updated = apply_activation_to_map(&descriptor, &activation, &user);
                let backup_path = self.write_backup(&plan.id, &user)?;
                if let Err(error) = write_user_environment(&updated) {
                    let _ = write_user_environment(&user);
                    return Err(AppError::Message(format!(
                        "写入用户环境失败，已尝试从 {} 回滚：{error}",
                        backup_path.display()
                    )));
                }
                let actual = read_environment(EnvironmentScope::User)?;
                if environment_fingerprint(&actual) != environment_fingerprint(&updated) {
                    let _ = write_user_environment(&user);
                    return Err(AppError::Message(
                        "写入后的环境快照与计划不一致，已回滚".to_string(),
                    ));
                }
                broadcast_environment_change();
                self.log_environment_operation(plan, "environment_switch_committed");
                Ok(())
            }
            PlannedAction::RestoreEnvironment { backup_path } => {
                let backup = serde_json::from_slice::<EnvironmentBackup>(&fs::read(backup_path)?)?;
                if backup.schema_version != 1 {
                    return Err(AppError::Message("不支持的环境备份版本".to_string()));
                }
                let safety_backup = self.write_backup(&plan.id, &user)?;
                if let Err(error) = write_user_environment(&backup.user_environment) {
                    let _ = write_user_environment(&user);
                    return Err(AppError::Message(format!(
                        "恢复失败，已尝试从 {} 回滚：{error}",
                        safety_backup.display()
                    )));
                }
                let actual = read_environment(EnvironmentScope::User)?;
                if environment_fingerprint(&actual)
                    != environment_fingerprint(&backup.user_environment)
                {
                    let _ = write_user_environment(&user);
                    return Err(AppError::Message(
                        "恢复后的环境快照与备份不一致，已回滚".to_string(),
                    ));
                }
                broadcast_environment_change();
                self.log_environment_operation(plan, "environment_restore_committed");
                Ok(())
            }
            PlannedAction::UpdateUserEnvironment { updated, reason } => {
                let backup_path = self.write_backup(&plan.id, &user)?;
                if let Err(error) = write_user_environment(updated) {
                    let _ = write_user_environment(&user);
                    return Err(AppError::Message(format!(
                        "诊断修复写入失败，已尝试从 {} 回滚：{error}",
                        backup_path.display()
                    )));
                }
                let actual = read_environment(EnvironmentScope::User)?;
                if environment_fingerprint(&actual) != environment_fingerprint(updated) {
                    let _ = write_user_environment(&user);
                    return Err(AppError::Message(
                        "诊断修复后的环境快照与计划不一致，已回滚".to_string(),
                    ));
                }
                broadcast_environment_change();
                self.log_environment_operation(
                    plan,
                    &format!("diagnostic_environment_repair_committed: {reason}"),
                );
                Ok(())
            }
            _ => Err(AppError::Message("该计划动作尚未实现".to_string())),
        }
    }

    pub fn apply_uninstall_environment(
        &self,
        plan: &OperationPlan,
    ) -> AppResult<Option<EnvironmentMap>> {
        let PlannedAction::Uninstall {
            user_environment_after,
            ..
        } = &plan.action
        else {
            return Err(AppError::InvalidPlan);
        };
        let Some(updated) = user_environment_after else {
            return Ok(None);
        };
        let current = read_environment(EnvironmentScope::User)?;
        let system = read_environment(EnvironmentScope::System)?;
        if combined_fingerprint(&current, &system) != plan.environment_fingerprint {
            return Err(AppError::StaleEnvironment);
        }
        let backup_path = self.write_backup(&plan.id, &current)?;
        if let Err(error) = write_user_environment(updated) {
            let _ = write_user_environment(&current);
            return Err(AppError::Message(format!(
                "卸载前清理用户环境失败，已尝试从 {} 回滚：{error}",
                backup_path.display()
            )));
        }
        let actual = read_environment(EnvironmentScope::User)?;
        if environment_fingerprint(&actual) != environment_fingerprint(updated) {
            let _ = write_user_environment(&current);
            return Err(AppError::Message(
                "卸载前的环境清理结果与计划不一致，已回滚".to_string(),
            ));
        }
        broadcast_environment_change();
        self.log_environment_operation(plan, "uninstall_environment_cleanup_committed");
        Ok(Some(current))
    }

    pub fn rollback_user_environment(&self, environment: &EnvironmentMap) -> AppResult<()> {
        write_user_environment(environment)?;
        let actual = read_environment(EnvironmentScope::User)?;
        if environment_fingerprint(&actual) != environment_fingerprint(environment) {
            return Err(AppError::Message(
                "卸载失败后的用户环境回滚校验未通过".to_string(),
            ));
        }
        broadcast_environment_change();
        Ok(())
    }

    fn log_environment_operation(&self, plan: &OperationPlan, event: &str) {
        let log_path = self
            .data_root
            .join("logs")
            .join(format!("operations-{}.jsonl", Utc::now().format("%Y-%m")));
        let value = serde_json::json!({
            "timestamp": Utc::now(),
            "operationId": plan.id,
            "level": "INFO",
            "event": event,
            "path": "HKCU\\Environment",
        });
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
            let _ = writeln!(file, "{value}");
        }
    }

    fn write_backup(&self, operation_id: &str, environment: &EnvironmentMap) -> AppResult<PathBuf> {
        let directory = self.data_root.join("backups").join("environment");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!(
            "{}-{}.json",
            Utc::now().format("%Y%m%dT%H%M%SZ"),
            operation_id
        ));
        let backup = EnvironmentBackup {
            schema_version: 1,
            created_at: Utc::now(),
            operation_id: operation_id.to_string(),
            user_environment: environment.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&backup)?;
        atomic_write(&path, &bytes)?;
        Ok(path)
    }
}

fn diagnostic_environment_update(
    issue_code: &str,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
) -> AppResult<(EnvironmentMap, String, Vec<String>)> {
    let mut updated = user.clone();
    let protected = protected_manager_path_keys(user, system);
    let current_path = split_path(get_case_insensitive(user, "PATH"));
    let (remaining_path, summary, warnings) = match issue_code {
        "PATH_DUPLICATE_用户" => {
            let mut seen = HashSet::new();
            let remaining = current_path
                .iter()
                .filter(|entry| seen.insert(normalize_key(entry)))
                .cloned()
                .collect::<Vec<_>>();
            (
                remaining,
                "删除用户 PATH 中后出现的重复条目，保留每个路径的第一次出现".to_string(),
                Vec::new(),
            )
        }
        "PATH_MISSING_用户" => {
            let mut protected_missing = Vec::new();
            let remaining = current_path
                .iter()
                .filter(|entry| {
                    let unquoted = entry.trim_matches('"');
                    if unquoted.is_empty()
                        || unquoted.contains('%')
                        || !Path::new(unquoted).is_absolute()
                    {
                        return true;
                    }
                    if Path::new(unquoted).exists() {
                        return true;
                    }
                    if path_matches_protected_manager(entry, &protected) {
                        protected_missing.push((*entry).clone());
                        return true;
                    }
                    false
                })
                .cloned()
                .collect::<Vec<_>>();
            let warnings = (!protected_missing.is_empty())
                .then(|| {
                    format!(
                        "以下版本管理器相关路径即使当前不存在也被保留：{}",
                        protected_missing.join(" | ")
                    )
                })
                .into_iter()
                .collect();
            (
                remaining,
                "删除用户 PATH 中不存在的普通绝对目录".to_string(),
                warnings,
            )
        }
        "PATH_EMPTY_用户" => (
            current_path
                .iter()
                .filter(|entry| !entry.is_empty())
                .cloned()
                .collect(),
            "删除用户 PATH 中会隐式引用当前目录的空条目".to_string(),
            Vec::new(),
        ),
        "PATH_RELATIVE_用户" => (
            current_path
                .iter()
                .filter(|entry| {
                    let unquoted = entry.trim_matches('"');
                    unquoted.contains('%')
                        || unquoted.is_empty()
                        || Path::new(unquoted).is_absolute()
                })
                .cloned()
                .collect(),
            "删除用户 PATH 中的相对或被截断条目".to_string(),
            vec!["相对条目可能源于未转义的分号；请在差异预览中核对被删除内容。".to_string()],
        ),
        code if code.starts_with("ENV_DUPLICATE_SCOPE_") => {
            let name = code.trim_start_matches("ENV_DUPLICATE_SCOPE_");
            let user_value = get_case_insensitive(user, name)
                .ok_or_else(|| AppError::Message(format!("用户级 {name} 已不存在")))?;
            let system_value = get_case_insensitive(system, name)
                .ok_or_else(|| AppError::Message(format!("系统级 {name} 已不存在")))?;
            if !user_value.eq_ignore_ascii_case(system_value) {
                return Err(AppError::Message(format!(
                    "{name} 在用户级和系统级的值不同，不能自动判断应保留哪一个；请查看 AI 分析或手动处理"
                )));
            }
            remove_case_insensitive(&mut updated, name);
            return Ok((
                updated,
                format!("删除与系统级值完全相同的用户变量 {name}"),
                vec![format!(
                    "系统级 {name} 保持不变；版本管理器仍会读取相同值。"
                )],
            ));
        }
        _ => {
            return Err(AppError::Message(
                "该诊断项不能安全地自动修复，请打开工具详情或使用 AI 分析".to_string(),
            ));
        }
    };

    if remaining_path != current_path {
        remove_case_insensitive(&mut updated, "PATH");
        updated.insert("Path".to_string(), remaining_path.join(";"));
    }
    Ok((updated, summary, warnings))
}

fn protected_manager_path_keys(user: &EnvironmentMap, system: &EnvironmentMap) -> HashSet<String> {
    let mut protected = HashSet::new();
    for name in [
        "PYENV_ROOT",
        "PYENV",
        "NVM_HOME",
        "NVM_SYMLINK",
        "FNM_DIR",
        "FNM_MULTISHELL_PATH",
        "VOLTA_HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "JABBA_HOME",
        "GOENV_ROOT",
    ] {
        for environment in [user, system] {
            let Some(value) = get_case_insensitive(environment, name) else {
                continue;
            };
            let root = PathBuf::from(value.trim_matches('"'));
            protected.insert(normalize_key(&root.to_string_lossy()));
            for child in ["bin", "shims", "current"] {
                protected.insert(normalize_key(&root.join(child).to_string_lossy()));
            }
        }
    }
    protected
}

fn path_matches_protected_manager(entry: &str, protected: &HashSet<String>) -> bool {
    let entry = normalize_key(entry);
    protected
        .iter()
        .any(|root| entry == *root || entry.starts_with(&format!("{root}\\")))
}

fn compare_environment_maps(
    before: &EnvironmentMap,
    after: &EnvironmentMap,
) -> Vec<EnvironmentDiff> {
    let mut names = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    names.sort_by_key(|name| name.to_ascii_uppercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    let mut diffs = Vec::new();
    for name in names {
        let old = get_case_insensitive(before, &name).cloned();
        let new = get_case_insensitive(after, &name).cloned();
        if old == new {
            continue;
        }
        let (added, removed) = if name.eq_ignore_ascii_case("PATH") {
            let old_entries = split_path(old.as_ref());
            let new_entries = split_path(new.as_ref());
            let old_keys = old_entries
                .iter()
                .map(|entry| normalize_key(entry))
                .collect::<HashSet<_>>();
            let new_keys = new_entries
                .iter()
                .map(|entry| normalize_key(entry))
                .collect::<HashSet<_>>();
            (
                new_entries
                    .into_iter()
                    .filter(|entry| !old_keys.contains(&normalize_key(entry)))
                    .collect(),
                old_entries
                    .into_iter()
                    .filter(|entry| !new_keys.contains(&normalize_key(entry)))
                    .collect(),
            )
        } else {
            (
                new.clone().into_iter().collect(),
                old.clone().into_iter().collect(),
            )
        };
        diffs.push(EnvironmentDiff {
            scope: EnvironmentScope::User,
            variable: name,
            before: old,
            after: new,
            added,
            removed,
        });
    }
    diffs
}

fn environment_with_command_directory(
    current: &EnvironmentMap,
    command_directory: &Path,
    enable: bool,
) -> EnvironmentMap {
    let command_key = normalize_key(&command_directory.to_string_lossy());
    let current_path = split_path(get_case_insensitive(current, "PATH"));
    let mut updated_path = current_path
        .iter()
        .filter(|entry| normalize_key(entry) != command_key)
        .cloned()
        .collect::<Vec<_>>();
    if enable {
        updated_path.insert(0, command_directory.to_string_lossy().into_owned());
    }
    if updated_path == current_path {
        return current.clone();
    }
    let mut updated = current.clone();
    remove_case_insensitive(&mut updated, "PATH");
    updated.insert("Path".to_string(), updated_path.join(";"));
    updated
}

fn command_script_conflicts(
    command_directory: &Path,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
) -> Vec<DiagnosticIssue> {
    let script_names = fs::read_dir(command_directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            path.extension()
                .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("cmd"))
                .then(|| entry.file_name())
        })
        .collect::<Vec<_>>();
    let command_key = normalize_key(&command_directory.to_string_lossy());
    let mut conflicts = Vec::new();
    for (scope, values, level) in [
        ("用户", user, IssueLevel::Warning),
        ("系统", system, IssueLevel::Error),
    ] {
        for directory in split_path(get_case_insensitive(values, "PATH")) {
            if directory.is_empty()
                || directory.contains('%')
                || normalize_key(&directory) == command_key
            {
                continue;
            }
            for script_name in &script_names {
                let candidate = Path::new(directory.trim_matches('"')).join(script_name);
                if candidate.is_file() {
                    let script = script_name.to_string_lossy();
                    conflicts.push(DiagnosticIssue {
                        code: format!(
                            "TERMINAL_COMMAND_{}_COLLISION",
                            script
                                .trim_end_matches(".cmd")
                                .replace('-', "_")
                                .to_ascii_uppercase()
                        ),
                        level,
                        title: format!("{scope} PATH 中已存在同名命令 {script}"),
                        detail: if level == IssueLevel::Error {
                            "系统 PATH 通常先于用户 PATH 合并，仅修改用户 PATH 无法保证 EnvNexus AI 脚本生效。"
                                .to_string()
                        } else {
                            "EnvNexus AI 命令目录会放到用户 PATH 最前；启用后将优先使用 EnvNexus AI 脚本。"
                                .to_string()
                        },
                        evidence: Some(candidate.display().to_string()),
                        repairable: false,
                    });
                }
            }
        }
    }
    conflicts
}

fn environment_after_uninstall(
    descriptor: &ToolDescriptor,
    installation_path: &Path,
    current: &EnvironmentMap,
) -> EnvironmentMap {
    let mut updated = current.clone();
    let remaining_path = split_path(get_case_insensitive(current, "PATH"))
        .into_iter()
        .filter(|entry| !path_is_same_or_below(entry, installation_path))
        .collect::<Vec<_>>();
    if remaining_path != split_path(get_case_insensitive(current, "PATH")) {
        remove_case_insensitive(&mut updated, "PATH");
        updated.insert("Path".to_string(), remaining_path.join(";"));
    }
    for variable in descriptor.home_variables {
        if get_case_insensitive(current, variable)
            .is_some_and(|value| path_is_same_or_below(value, installation_path))
        {
            remove_case_insensitive(&mut updated, variable);
        }
    }
    updated
}

fn path_is_same_or_below(value: &str, root: &Path) -> bool {
    if value.contains('%') {
        return false;
    }
    let value = normalize_key(value);
    let root = normalize_key(&root.to_string_lossy());
    value == root || value.starts_with(&format!("{root}\\"))
}

fn installation_destination(tool_id: &str, version: &str, root: &Path) -> AppResult<PathBuf> {
    let safe_version = sanitize_component(version)?;
    let destination = match tool_id {
        "android-sdk" => root.join("cmdline-tools").join("latest"),
        "android-ndk" => root.join("ndk").join(safe_version),
        "adb" => root.join("platform-tools"),
        _ => root.join(tool_id).join(safe_version),
    };
    Ok(destination)
}

fn sanitize_component(value: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.chars().any(|character| {
            matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\0'
            )
        })
    {
        return Err(AppError::InvalidSource(format!(
            "版本号不能安全用作目录名：{value}"
        )));
    }
    Ok(trimmed.to_string())
}

fn validate_download_url(tool_id: &str, value: &str) -> AppResult<()> {
    let url = url::Url::parse(value)
        .map_err(|error| AppError::InvalidSource(format!("下载 URL 无效：{error}")))?;
    if url.scheme() != "https" {
        return Err(AppError::InvalidSource("下载 URL 不是 HTTPS".to_string()));
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let allowed = match tool_id {
        "python" => ["www.python.org", "python.org"].as_slice(),
        "java" => ["github.com", "api.adoptium.net"].as_slice(),
        "go" => ["go.dev", "dl.google.com"].as_slice(),
        "rust" => ["static.rust-lang.org"].as_slice(),
        "node" => ["nodejs.org"].as_slice(),
        "git" | "cmake" => ["github.com", "objects.githubusercontent.com"].as_slice(),
        "maven" => ["downloads.apache.org"].as_slice(),
        "dotnet" => [
            "builds.dotnet.microsoft.com",
            "download.visualstudio.microsoft.com",
        ]
        .as_slice(),
        "ruby" => [
            "github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com",
        ]
        .as_slice(),
        "php" => ["windows.php.net"].as_slice(),
        "android-sdk" | "android-ndk" | "adb" => ["dl.google.com"].as_slice(),
        "gradle" => [
            "services.gradle.org",
            "github.com",
            "objects.githubusercontent.com",
        ]
        .as_slice(),
        _ => &[],
    };
    if !allowed.iter().any(|candidate| host == *candidate) {
        return Err(AppError::InvalidSource(format!(
            "{tool_id} 不允许从主机 {host} 下载"
        )));
    }
    Ok(())
}

fn source_host(value: &str) -> String {
    url::Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| "官方源".to_string())
}

#[derive(Debug)]
struct Activation {
    variables: BTreeMap<String, String>,
    path_entries: Vec<PathBuf>,
}

fn activation_for(descriptor: &ToolDescriptor, root: &Path) -> AppResult<Activation> {
    let mut variables = BTreeMap::new();
    let path_entries = match descriptor.id {
        "python" => {
            require_file(root.join("python.exe"))?;
            vec![root.to_path_buf(), root.join("Scripts")]
        }
        "java" => {
            require_file(root.join("bin").join("java.exe"))?;
            variables.insert("JAVA_HOME".to_string(), root.to_string_lossy().into_owned());
            vec![root.join("bin")]
        }
        "go" => {
            require_file(root.join("bin").join("go.exe"))?;
            variables.insert("GOROOT".to_string(), root.to_string_lossy().into_owned());
            vec![root.join("bin")]
        }
        "rust" => {
            let managed_cargo = root.join("cargo");
            let managed_rustup = root.join("rustup");
            if managed_cargo.join("bin").join("rustc.exe").is_file() {
                variables.insert(
                    "CARGO_HOME".to_string(),
                    managed_cargo.to_string_lossy().into_owned(),
                );
                variables.insert(
                    "RUSTUP_HOME".to_string(),
                    managed_rustup.to_string_lossy().into_owned(),
                );
                vec![managed_cargo.join("bin")]
            } else {
                require_file(root.join("bin").join("rustc.exe"))?;
                vec![root.join("bin")]
            }
        }
        "node" => {
            require_file(root.join("node.exe"))?;
            vec![root.to_path_buf()]
        }
        "git" => {
            let command = if root.join("cmd").join("git.exe").is_file() {
                root.join("cmd")
            } else {
                root.join("bin")
            };
            require_file(command.join("git.exe"))?;
            vec![command]
        }
        "android-sdk" => {
            let sdk_root = if root.join("bin").join("sdkmanager.bat").is_file()
                && root
                    .parent()
                    .is_some_and(|parent| parent.ends_with("cmdline-tools"))
            {
                root.parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| AppError::UnsafePath(root.to_path_buf()))?
            } else {
                root
            };
            let command_line = sdk_root.join("cmdline-tools").join("latest").join("bin");
            require_file(command_line.join("sdkmanager.bat"))?;
            variables.insert(
                "ANDROID_HOME".to_string(),
                sdk_root.to_string_lossy().into_owned(),
            );
            variables.insert(
                "ANDROID_SDK_ROOT".to_string(),
                sdk_root.to_string_lossy().into_owned(),
            );
            vec![command_line, sdk_root.join("platform-tools")]
        }
        "android-ndk" => {
            require_file(root.join("ndk-build.cmd"))?;
            variables.insert(
                "ANDROID_NDK_HOME".to_string(),
                root.to_string_lossy().into_owned(),
            );
            vec![root.to_path_buf()]
        }
        "gradle" => {
            require_file(root.join("bin").join("gradle.bat"))?;
            variables.insert(
                "GRADLE_HOME".to_string(),
                root.to_string_lossy().into_owned(),
            );
            vec![root.join("bin")]
        }
        "cmake" => {
            require_file(root.join("bin").join("cmake.exe"))?;
            vec![root.join("bin")]
        }
        "maven" => {
            require_file(root.join("bin").join("mvn.cmd"))?;
            let value = root.to_string_lossy().into_owned();
            variables.insert("MAVEN_HOME".to_string(), value.clone());
            variables.insert("M2_HOME".to_string(), value);
            vec![root.join("bin")]
        }
        "dotnet" => {
            require_file(root.join("dotnet.exe"))?;
            let value = root.to_string_lossy().into_owned();
            variables.insert("DOTNET_ROOT".to_string(), value.clone());
            variables.insert("DOTNET_ROOT_X64".to_string(), value);
            vec![root.to_path_buf()]
        }
        "ruby" => {
            require_file(root.join("bin").join("ruby.exe"))?;
            variables.insert("RUBY_HOME".to_string(), root.to_string_lossy().into_owned());
            vec![root.join("bin")]
        }
        "php" => {
            require_file(root.join("php.exe"))?;
            variables.insert("PHP_HOME".to_string(), root.to_string_lossy().into_owned());
            vec![root.to_path_buf()]
        }
        "adb" => {
            let sdk_root = if root.join("adb.exe").is_file() && root.ends_with("platform-tools") {
                root.parent()
                    .ok_or_else(|| AppError::UnsafePath(root.to_path_buf()))?
            } else {
                root
            };
            let platform_tools = sdk_root.join("platform-tools");
            require_file(platform_tools.join("adb.exe"))?;
            variables.insert(
                "ANDROID_HOME".to_string(),
                sdk_root.to_string_lossy().into_owned(),
            );
            variables.insert(
                "ANDROID_SDK_ROOT".to_string(),
                sdk_root.to_string_lossy().into_owned(),
            );
            vec![platform_tools]
        }
        _ => return Err(AppError::UnknownTool(descriptor.id.to_string())),
    };
    Ok(Activation {
        variables,
        path_entries: path_entries
            .into_iter()
            .filter(|path| path.is_dir())
            .collect(),
    })
}

fn require_file(path: PathBuf) -> AppResult<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(AppError::Message(format!(
            "所选目录不包含预期文件：{}",
            path.display()
        )))
    }
}

fn build_environment_diffs(
    descriptor: &ToolDescriptor,
    activation: &Activation,
    current: &EnvironmentMap,
) -> (Vec<EnvironmentDiff>, Vec<String>, Vec<String>) {
    let updated = apply_activation_to_map(descriptor, activation, current);
    let before_path = split_path(get_case_insensitive(current, "PATH"));
    let after_path = split_path(get_case_insensitive(&updated, "PATH"));
    let before_keys = before_path
        .iter()
        .map(|value| normalize_key(value))
        .collect::<HashSet<_>>();
    let after_keys = after_path
        .iter()
        .map(|value| normalize_key(value))
        .collect::<HashSet<_>>();
    let added = after_path
        .iter()
        .filter(|value| !before_keys.contains(&normalize_key(value)))
        .cloned()
        .collect::<Vec<_>>();
    let removed = before_path
        .iter()
        .filter(|value| !after_keys.contains(&normalize_key(value)))
        .cloned()
        .collect::<Vec<_>>();
    let mut diffs = vec![EnvironmentDiff {
        scope: EnvironmentScope::User,
        variable: "Path".to_string(),
        before: get_case_insensitive(current, "PATH").cloned(),
        after: get_case_insensitive(&updated, "PATH").cloned(),
        added: added.clone(),
        removed: removed.clone(),
    }];
    for (name, value) in &activation.variables {
        let before = get_case_insensitive(current, name).cloned();
        if before.as_deref() == Some(value) {
            continue;
        }
        diffs.push(EnvironmentDiff {
            scope: EnvironmentScope::User,
            variable: name.clone(),
            before: before.clone(),
            after: Some(value.clone()),
            added: vec![value.clone()],
            removed: before.into_iter().collect(),
        });
    }
    (diffs, added, removed)
}

fn apply_activation_to_map(
    descriptor: &ToolDescriptor,
    activation: &Activation,
    current: &EnvironmentMap,
) -> EnvironmentMap {
    let mut updated = current.clone();
    for (name, value) in &activation.variables {
        remove_case_insensitive(&mut updated, name);
        updated.insert(name.clone(), value.clone());
    }
    let current_path = split_path(get_case_insensitive(current, "PATH"));
    let mut new_path = activation
        .path_entries
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    new_path.extend(
        current_path
            .into_iter()
            .filter(|entry| !entry.is_empty())
            .filter(|entry| !is_tool_path_entry(descriptor, entry))
            .filter(|entry| {
                !activation
                    .path_entries
                    .iter()
                    .any(|added| normalize_key(entry) == normalize_key(&added.to_string_lossy()))
            }),
    );
    remove_case_insensitive(&mut updated, "PATH");
    updated.insert("Path".to_string(), new_path.join(";"));
    updated
}

fn system_path_conflicts(
    descriptor: &ToolDescriptor,
    selected_root: &Path,
    system: &EnvironmentMap,
) -> Vec<DiagnosticIssue> {
    split_path(get_case_insensitive(system, "PATH"))
        .into_iter()
        .filter(|entry| !entry.is_empty() && !entry.contains('%'))
        .filter(|entry| is_tool_path_entry(descriptor, entry))
        .filter(|entry| {
            crate::paths::canonicalize_simplified(Path::new(entry.trim_matches('"')))
                .ok()
                .is_none_or(|entry| !entry.starts_with(selected_root))
        })
        .map(|entry| DiagnosticIssue {
            code: format!(
                "{}_SYSTEM_PATH_PRECEDENCE",
                descriptor.id.to_ascii_uppercase()
            ),
            level: IssueLevel::Error,
            title: "系统 PATH 会遮蔽所选用户版本".to_string(),
            detail: "仅修改用户 PATH 无法保证默认版本。系统级变更需要单独的提权计划和确认。"
                .to_string(),
            evidence: Some(entry),
            repairable: false,
        })
        .collect()
}

fn is_tool_path_entry(descriptor: &ToolDescriptor, entry: &str) -> bool {
    let path = Path::new(entry.trim_matches('"'));
    if path.join(descriptor.executable).is_file() {
        return true;
    }
    match descriptor.id {
        "android-sdk" => path.join("sdkmanager.bat").is_file(),
        "android-ndk" => path.join("ndk-build.cmd").is_file(),
        "adb" => path.join("adb.exe").is_file(),
        _ => false,
    }
}

fn remove_case_insensitive(values: &mut EnvironmentMap, name: &str) {
    let existing = values
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned();
    if let Some(existing) = existing {
        values.remove(&existing);
    }
}

fn normalize_key(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_start_matches(r"\\?\")
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn combined_fingerprint(user: &EnvironmentMap, system: &EnvironmentMap) -> String {
    let mut hasher = Sha256::new();
    hasher.update(environment_fingerprint(user));
    hasher.update(environment_fingerprint(system));
    hex::encode(hasher.finalize())
}

fn confirmation_token(id: &str, fingerprint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update(fingerprint.as_bytes());
    hasher.update(Uuid::new_v4().as_bytes());
    hex::encode(hasher.finalize())[..24].to_string()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)?;
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

#[cfg(windows)]
fn write_user_environment(values: &EnvironmentMap) -> AppResult<()> {
    use winreg::{RegValue, enums::{KEY_READ, RegType}, types::FromRegValue};

    let root = RegKey::predef(HKEY_CURRENT_USER);
    let key = root
        .open_subkey_with_flags("Environment", KEY_READ | KEY_SET_VALUE)
        .map_err(|error| AppError::Message(format!("打开 HKCU\\Environment 失败：{error}")))?;
    // 记录现有字符串值的名称、注册表类型和原文，
    // 非字符串值（REG_DWORD 等）不属于本工具管理范围，保持原样。
    let mut current = Vec::new();
    for entry in key.enum_values() {
        let Ok((name, raw)) = entry else {
            continue;
        };
        let Ok(text) = String::from_reg_value(&raw) else {
            continue;
        };
        current.push((name, raw.vtype, text));
    }
    for (name, _, _) in &current {
        if !values
            .keys()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
        {
            key.delete_value(name).map_err(|error| {
                AppError::Message(format!("删除用户环境变量 {name} 失败：{error}"))
            })?;
        }
    }
    for (name, value) in values {
        let existing = current
            .iter()
            .find(|(existing_name, _, _)| existing_name.eq_ignore_ascii_case(name));
        // 值未变化时不重写，保留原始注册表类型和名称大小写。
        if existing.is_some_and(|(_, _, text)| text == value) {
            continue;
        }
        // Windows 只对 REG_EXPAND_SZ 展开 %VAR%；沿用既有类型，新值含 % 时按可展开类型写入。
        let vtype = match existing {
            Some((_, RegType::REG_EXPAND_SZ, _)) => RegType::REG_EXPAND_SZ,
            Some((_, RegType::REG_SZ, _)) => RegType::REG_SZ,
            _ => {
                if value.contains('%') {
                    RegType::REG_EXPAND_SZ
                } else {
                    RegType::REG_SZ
                }
            }
        };
        let bytes = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<u8>>();
        key.set_raw_value(name, &RegValue { bytes, vtype })
            .map_err(|error| AppError::Message(format!("写入用户环境变量 {name} 失败：{error}")))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn write_user_environment(_values: &EnvironmentMap) -> AppResult<()> {
    Err(AppError::Message("环境写入只支持 Windows".to_string()))
}

#[cfg(windows)]
fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    let wide = "Environment\0".encode_utf16().collect::<Vec<_>>();
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            wide.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5_000,
            &mut result,
        );
    }
}

#[cfg(not(windows))]
fn broadcast_environment_change() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::PluginRegistry;
    use tempfile::tempdir;

    #[test]
    fn selected_entries_are_added_first_and_old_tool_entries_removed() {
        let temp = tempdir().unwrap();
        let selected = temp.path().join("node-22");
        let old = temp.path().join("node-20");
        fs::create_dir_all(&selected).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::write(selected.join("node.exe"), b"").unwrap();
        fs::write(old.join("node.exe"), b"").unwrap();
        let registry = PluginRegistry::builtin();
        let descriptor = registry.get("node").unwrap();
        let activation = activation_for(descriptor.descriptor(), &selected).unwrap();
        let mut current = EnvironmentMap::new();
        current.insert(
            "Path".into(),
            format!("{};C:\\Windows", old.to_string_lossy()),
        );
        let updated = apply_activation_to_map(descriptor.descriptor(), &activation, &current);
        let entries = split_path(get_case_insensitive(&updated, "PATH"));
        assert_eq!(Path::new(&entries[0]), selected);
        assert!(!entries.iter().any(|entry| Path::new(entry) == old));
        assert!(entries.iter().any(|entry| entry == r"C:\Windows"));
    }

    #[test]
    fn managed_rust_activation_uses_isolated_cargo_and_rustup_homes() {
        let temp = tempdir().unwrap();
        let installation = temp.path().join("rust").join("1.97.1");
        fs::create_dir_all(installation.join("cargo").join("bin")).unwrap();
        fs::create_dir_all(installation.join("rustup")).unwrap();
        fs::write(
            installation.join("cargo").join("bin").join("rustc.exe"),
            b"",
        )
        .unwrap();
        let registry = PluginRegistry::builtin();
        let descriptor = registry.get("rust").unwrap();
        let activation = activation_for(descriptor.descriptor(), &installation).unwrap();
        assert_eq!(
            activation.variables.get("CARGO_HOME"),
            Some(&installation.join("cargo").to_string_lossy().into_owned())
        );
        assert_eq!(
            activation.variables.get("RUSTUP_HOME"),
            Some(&installation.join("rustup").to_string_lossy().into_owned())
        );
        assert_eq!(
            activation.path_entries,
            vec![installation.join("cargo").join("bin")]
        );
    }

    #[test]
    fn android_component_directories_activate_the_shared_sdk_root() {
        let temp = tempdir().unwrap();
        let sdk = temp.path().join("Android");
        let command_line = sdk.join("cmdline-tools").join("latest");
        let platform_tools = sdk.join("platform-tools");
        fs::create_dir_all(command_line.join("bin")).unwrap();
        fs::create_dir_all(&platform_tools).unwrap();
        fs::write(command_line.join("bin").join("sdkmanager.bat"), b"").unwrap();
        fs::write(platform_tools.join("adb.exe"), b"").unwrap();
        let registry = PluginRegistry::builtin();

        let sdk_plugin = registry.get("android-sdk").unwrap();
        let sdk_activation =
            activation_for(sdk_plugin.descriptor(), command_line.as_path()).unwrap();
        assert_eq!(
            sdk_activation.variables.get("ANDROID_HOME"),
            Some(&sdk.to_string_lossy().into_owned())
        );

        let adb_plugin = registry.get("adb").unwrap();
        let adb_activation =
            activation_for(adb_plugin.descriptor(), platform_tools.as_path()).unwrap();
        assert_eq!(
            adb_activation.variables.get("ANDROID_SDK_ROOT"),
            Some(&sdk.to_string_lossy().into_owned())
        );
        assert_eq!(adb_activation.path_entries, vec![platform_tools]);
    }

    #[test]
    fn command_directory_can_be_enabled_and_removed_without_touching_other_path_entries() {
        let command_directory = PathBuf::from(r"E:\EnvNexus AI Data\commands");
        let mut current = EnvironmentMap::new();
        current.insert(
            "Path".to_string(),
            format!(
                r"C:\Windows;{};{}",
                command_directory.display(),
                command_directory.display()
            ),
        );

        let enabled = environment_with_command_directory(&current, &command_directory, true);
        let enabled_entries = split_path(get_case_insensitive(&enabled, "PATH"));
        assert_eq!(Path::new(&enabled_entries[0]), command_directory);
        assert_eq!(
            enabled_entries
                .iter()
                .filter(|entry| {
                    normalize_key(entry) == normalize_key(&command_directory.to_string_lossy())
                })
                .count(),
            1
        );

        let disabled = environment_with_command_directory(&enabled, &command_directory, false);
        let disabled_entries = split_path(get_case_insensitive(&disabled, "PATH"));
        assert_eq!(disabled_entries, vec![r"C:\Windows".to_string()]);
    }

    #[test]
    fn command_directory_preview_detects_user_and_system_name_collisions() {
        let temp = tempdir().unwrap();
        let command_directory = temp.path().join("commands");
        let user_directory = temp.path().join("user-bin");
        let system_directory = temp.path().join("system-bin");
        fs::create_dir_all(&command_directory).unwrap();
        fs::create_dir_all(&user_directory).unwrap();
        fs::create_dir_all(&system_directory).unwrap();
        fs::write(command_directory.join("jdk-list.cmd"), b"").unwrap();
        fs::write(user_directory.join("jdk-list.cmd"), b"").unwrap();
        fs::write(system_directory.join("jdk-list.cmd"), b"").unwrap();
        let user = EnvironmentMap::from([(
            "Path".to_string(),
            user_directory.to_string_lossy().into_owned(),
        )]);
        let system = EnvironmentMap::from([(
            "Path".to_string(),
            system_directory.to_string_lossy().into_owned(),
        )]);

        let conflicts = command_script_conflicts(&command_directory, &user, &system);
        assert_eq!(conflicts.len(), 2);
        assert!(
            conflicts
                .iter()
                .any(|conflict| conflict.level == IssueLevel::Warning)
        );
        assert!(
            conflicts
                .iter()
                .any(|conflict| conflict.level == IssueLevel::Error)
        );
    }

    #[test]
    fn uninstall_environment_cleanup_only_removes_managed_version_references() {
        let registry = PluginRegistry::builtin();
        let descriptor = registry.get("java").unwrap();
        let installation = PathBuf::from(r"E:\Toolchains\java\21");
        let mut current = EnvironmentMap::new();
        current.insert(
            "Path".to_string(),
            format!(
                r"{}\bin;C:\Windows;E:\OtherJava\bin",
                installation.display()
            ),
        );
        current.insert(
            "JAVA_HOME".to_string(),
            installation.to_string_lossy().into_owned(),
        );
        current.insert("UNCHANGED".to_string(), "value".to_string());

        let updated = environment_after_uninstall(descriptor.descriptor(), &installation, &current);
        assert!(get_case_insensitive(&updated, "JAVA_HOME").is_none());
        assert_eq!(
            split_path(get_case_insensitive(&updated, "PATH")),
            vec![r"C:\Windows", r"E:\OtherJava\bin"]
        );
        assert_eq!(updated.get("UNCHANGED").map(String::as_str), Some("value"));
    }

    #[test]
    fn diagnostic_missing_path_cleanup_preserves_version_manager_roots() {
        let temp = tempdir().unwrap();
        let nvm_link = temp.path().join("nvm-current");
        let ordinary_missing = temp.path().join("ordinary-missing");
        let mut user = EnvironmentMap::new();
        user.insert(
            "Path".to_string(),
            format!("{};{}", nvm_link.display(), ordinary_missing.display()),
        );
        user.insert(
            "NVM_SYMLINK".to_string(),
            nvm_link.to_string_lossy().into_owned(),
        );

        let (updated, _, warnings) =
            diagnostic_environment_update("PATH_MISSING_用户", &user, &EnvironmentMap::new())
                .unwrap();
        let entries = split_path(get_case_insensitive(&updated, "PATH"));

        assert!(entries.iter().any(|entry| Path::new(entry) == nvm_link));
        assert!(
            !entries
                .iter()
                .any(|entry| Path::new(entry) == ordinary_missing)
        );
        assert!(!warnings.is_empty());
    }

    #[test]
    fn diagnostic_duplicate_path_cleanup_keeps_first_entry() {
        let temp = tempdir().unwrap();
        let first = temp.path().to_string_lossy().into_owned();
        let mut user = EnvironmentMap::new();
        user.insert("Path".to_string(), format!("{first};{first};C:\\Windows"));

        let (updated, _, _) =
            diagnostic_environment_update("PATH_DUPLICATE_用户", &user, &EnvironmentMap::new())
                .unwrap();
        let entries = split_path(get_case_insensitive(&updated, "PATH"));

        assert_eq!(entries.iter().filter(|entry| *entry == &first).count(), 1);
        assert_eq!(entries[0], first);
    }

    #[test]
    fn duplicate_scope_repair_only_removes_identical_user_value() {
        let mut user = EnvironmentMap::new();
        let mut system = EnvironmentMap::new();
        user.insert("NVM_HOME".to_string(), r"E:\Environment\nvm".to_string());
        system.insert("NVM_HOME".to_string(), r"E:\Environment\nvm".to_string());

        let (updated, _, _) =
            diagnostic_environment_update("ENV_DUPLICATE_SCOPE_NVM_HOME", &user, &system).unwrap();

        assert!(get_case_insensitive(&updated, "NVM_HOME").is_none());
        system.insert("NVM_HOME".to_string(), r"E:\Different\nvm".to_string());
        assert!(
            diagnostic_environment_update("ENV_DUPLICATE_SCOPE_NVM_HOME", &user, &system).is_err()
        );
    }
}
