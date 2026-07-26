use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    installer::{Installer, OperationResult},
    model::{EnvironmentScope, OperationPlan, PlannedAction, ToolInventory},
    plans::PlanService,
    plugins::PluginRegistry,
    scanner,
};

pub fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("无法启动 EnvNexus AI 命令运行时：{error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(std::env::args().skip(1).collect())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误：{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(mut args: Vec<String>) -> AppResult<()> {
    let json = remove_flag(&mut args, "--json");
    let confirmed = remove_flag(&mut args, "--yes");
    let data_root = crate::resolve_data_root();
    crate::ensure_data_layout(&data_root)?;
    let registry = PluginRegistry::builtin();

    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        print_help();
        return Ok(());
    }
    if let Some(alias) = args[0].strip_suffix("-list") {
        return list_tool(&registry, &data_root, normalize_tool_id(alias), json);
    }

    match args[0].as_str() {
        "tools" => list_definitions(&registry, &data_root, json),
        "command-scripts" => command_scripts(&registry, &data_root, &args, json),
        "scan" => {
            let scan = scanner::scan(&registry, &data_root)?;
            crate::write_cached_environment_scan(&data_root, &scan)?;
            if json {
                print_json(&scan)?;
            } else {
                println!(
                    "扫描完成：{} 个工具，{} 个全局问题，{} 个版本管理器",
                    scan.tools.len(),
                    scan.issues.len(),
                    scan.version_managers.len()
                );
                println!("快照：{}", scan_snapshot_path(&data_root).display());
            }
            Ok(())
        }
        "list" => {
            let tool_id = required_arg(&args, 1, "用法：EnvNexus-AI.exe list <tool> [--json]")?;
            list_tool(&registry, &data_root, normalize_tool_id(tool_id), json)
        }
        "diagnose" => list_diagnostics(&data_root, json),
        "root" => root_command(&registry, &data_root, &args, json),
        "versions" => {
            let tool_id = normalize_tool_id(required_arg(
                &args,
                1,
                "用法：EnvNexus-AI.exe versions <tool>",
            )?);
            let client = http_client()?;
            let catalog = registry
                .get(tool_id)?
                .fetch_available_versions(&client)
                .await?;
            if json {
                print_json(&catalog)
            } else {
                println!(
                    "{} 官方版本（{}，查询于 {}）：",
                    tool_id, catalog.source_name, catalog.fetched_at
                );
                for version in catalog.versions {
                    println!(
                        "  {:<18} {:<10} {}",
                        version.version, version.channel, version.architecture
                    );
                }
                Ok(())
            }
        }
        "install" => {
            let tool_id = normalize_tool_id(required_arg(
                &args,
                1,
                "用法：EnvNexus-AI.exe install <tool> <version> [--yes]",
            )?);
            let version = required_arg(
                &args,
                2,
                "用法：EnvNexus-AI.exe install <tool> <version> [--yes]",
            )?;
            install(&registry, &data_root, tool_id, version, confirmed, json).await
        }
        "use" => {
            let tool_id = normalize_tool_id(required_arg(
                &args,
                1,
                "用法：EnvNexus-AI.exe use <tool> <installation-path> [--yes]",
            )?);
            let path = PathBuf::from(required_arg(
                &args,
                2,
                "用法：EnvNexus-AI.exe use <tool> <installation-path> [--yes]",
            )?);
            let plans = PlanService::new(data_root.clone());
            let plan = plans.preview_switch(&registry, tool_id, path)?;
            apply_or_preview(&registry, &data_root, &plans, plan, confirmed, json).await
        }
        "repair" => {
            let tool_id = normalize_tool_id(required_arg(
                &args,
                1,
                "用法：EnvNexus-AI.exe repair <tool> <installation-path> [--yes]",
            )?);
            let path = PathBuf::from(required_arg(
                &args,
                2,
                "用法：EnvNexus-AI.exe repair <tool> <installation-path> [--yes]",
            )?);
            repair(&registry, &data_root, tool_id, path, confirmed, json).await
        }
        "uninstall" => {
            let tool_id = normalize_tool_id(required_arg(
                &args,
                1,
                "用法：EnvNexus-AI.exe uninstall <tool> <installation-path> [--yes]",
            )?);
            let path = PathBuf::from(required_arg(
                &args,
                2,
                "用法：EnvNexus-AI.exe uninstall <tool> <installation-path> [--yes]",
            )?);
            let plans = PlanService::new(data_root.clone());
            let plan = plans.preview_uninstall(&registry, tool_id, path)?;
            apply_or_preview(&registry, &data_root, &plans, plan, confirmed, json).await
        }
        "diagnostic-repair" => {
            let issue_code = required_arg(
                &args,
                1,
                "用法：EnvNexus-AI.exe diagnostic-repair <issue-code> [--yes]",
            )?;
            let plans = PlanService::new(data_root.clone());
            let plan = plans.preview_diagnostic_repair(issue_code)?;
            apply_or_preview(&registry, &data_root, &plans, plan, confirmed, json).await
        }
        command => Err(AppError::Message(format!(
            "未知命令 {command}；运行 EnvNexus-AI.exe help 查看用法"
        ))),
    }
}

fn command_scripts(
    registry: &PluginRegistry,
    data_root: &Path,
    args: &[String],
    json: bool,
) -> AppResult<()> {
    let action = args.get(1).map(String::as_str).unwrap_or("status");
    let status = match action {
        "status" => crate::terminal::status(registry, data_root)?,
        "prepare" => crate::terminal::prepare(registry, data_root)?,
        _ => {
            return Err(AppError::Message(
                "command-scripts 只支持 status 或 prepare".to_string(),
            ));
        }
    };
    if json {
        print_json(&status)
    } else {
        println!(
            "{}：{}/{} 个脚本；用户 PATH {}",
            status.directory.display(),
            status.script_count,
            status.expected_script_count,
            if status.enabled_in_user_path {
                "已启用"
            } else {
                "未启用"
            }
        );
        Ok(())
    }
}

fn list_definitions(registry: &PluginRegistry, data_root: &Path, json: bool) -> AppResult<()> {
    let preferences = crate::read_tool_root_preferences(data_root)?;
    let rows = registry
        .all()
        .iter()
        .map(|plugin| {
            let descriptor = plugin.descriptor();
            ToolDefinitionRow {
                id: descriptor.id,
                display_name: descriptor.display_name,
                category: descriptor.category,
                install_root: preferences.roots.get(descriptor.id),
            }
        })
        .collect::<Vec<_>>();
    if json {
        return print_json(&rows);
    }
    println!("EnvNexus AI 内置工具（无需扫描）：");
    for row in rows {
        println!(
            "  {:<14} {:<18} {:<10} {}",
            row.id,
            row.display_name,
            row.category,
            row.install_root
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "未设置目录".to_string())
        );
    }
    Ok(())
}

fn list_tool(
    registry: &PluginRegistry,
    data_root: &Path,
    tool_id: &str,
    json: bool,
) -> AppResult<()> {
    let plugin = registry.get(tool_id)?;
    let scan = crate::read_cached_environment_scan(data_root)?;
    let inventory = scan
        .as_ref()
        .and_then(|scan| scan.tools.iter().find(|tool| tool.id == tool_id));
    let preferences = crate::read_tool_root_preferences(data_root)?;
    if json {
        return print_json(&ToolListOutput {
            tool_id,
            display_name: plugin.descriptor().display_name,
            scan_finished_at: scan.as_ref().map(|scan| scan.scan_finished_at),
            install_root: preferences.roots.get(tool_id),
            inventory,
        });
    }

    println!("{} ({tool_id})", plugin.descriptor().display_name);
    println!(
        "  安装根目录：{}",
        preferences
            .roots
            .get(tool_id)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未设置".to_string())
    );
    let Some(scan) = scan.as_ref() else {
        println!("  扫描状态：尚无快照；list 命令不会自动扫描");
        println!("  可运行：env-scan（或 EnvNexus-AI.exe scan）");
        return Ok(());
    };
    println!("  快照时间：{}", scan.scan_finished_at);
    let Some(inventory) = inventory else {
        println!("  当前默认：未在快照中找到该工具");
        return Ok(());
    };
    println!(
        "  当前默认：{}",
        inventory
            .default_version
            .as_ref()
            .map(|version| format!("{} ({})", version.version, version.path.display()))
            .unwrap_or_else(|| "未安装或未解析到".to_string())
    );
    println!("  已安装版本：{}", inventory.installed_versions.len());
    for version in &inventory.installed_versions {
        println!(
            "    {} {:<16} {}{}",
            if version.is_default { "*" } else { " " },
            version.version,
            version.path.display(),
            if version.managed {
                " [EnvNexus AI]"
            } else {
                ""
            }
        );
    }
    Ok(())
}

fn list_diagnostics(data_root: &Path, json: bool) -> AppResult<()> {
    let scan = crate::read_cached_environment_scan(data_root)?.ok_or_else(|| {
        AppError::Message("尚无扫描快照；diagnose 不会自动扫描，请先运行 env-scan".to_string())
    })?;
    let issues = scan
        .issues
        .iter()
        .chain(scan.tools.iter().flat_map(|tool| tool.issues.iter()))
        .collect::<Vec<_>>();
    if json {
        return print_json(&issues);
    }
    let user = crate::environment::read_environment(EnvironmentScope::User)?;
    let system = crate::environment::read_environment(EnvironmentScope::System)?;
    let preferences = crate::read_tool_root_preferences(data_root)?;
    let machine = crate::diagnostics::machine_context(data_root, &preferences, &user, &system);
    println!(
        "{} 项诊断（快照 {}）：",
        issues.len(),
        scan.scan_finished_at
    );
    for issue in issues {
        let guidance = crate::diagnostics::guidance_for(issue, &scan, &machine, &user, &system);
        println!(
            "  [{:?}] {}  {}{}",
            issue.level,
            issue.code,
            issue.title,
            if issue.repairable {
                " [可生成修复计划]"
            } else {
                ""
            }
        );
        println!("    本地分析：{}", guidance.summary);
        for recommendation in guidance.recommendations.iter().take(2) {
            println!("    建议：{recommendation}");
        }
        for command in guidance.commands.iter().take(3) {
            println!("    {}：{}", command.label, command.command);
        }
    }
    Ok(())
}

fn root_command(
    registry: &PluginRegistry,
    data_root: &Path,
    args: &[String],
    json: bool,
) -> AppResult<()> {
    let action = required_arg(
        args,
        1,
        "用法：EnvNexus-AI.exe root get <tool> | root set <tool> <absolute-path>",
    )?;
    let tool_id = normalize_tool_id(required_arg(
        args,
        2,
        "用法：EnvNexus-AI.exe root get <tool> | root set <tool> <absolute-path>",
    )?);
    registry.get(tool_id)?;
    let mut preferences = crate::read_tool_root_preferences(data_root)?;
    match action {
        "get" => {
            let root = preferences.roots.get(tool_id);
            if json {
                print_json(&RootOutput { tool_id, root })
            } else {
                println!(
                    "{}",
                    root.map(|path| path.display().to_string())
                        .unwrap_or_else(|| "未设置".to_string())
                );
                Ok(())
            }
        }
        "set" => {
            let path = PathBuf::from(required_arg(
                args,
                3,
                "用法：EnvNexus-AI.exe root set <tool> <absolute-path>",
            )?);
            let path = crate::normalize_install_root(path).map_err(AppError::Message)?;
            if crate::ANDROID_WORKSPACE_TOOL_IDS.contains(&tool_id) {
                preferences.android_root = Some(path.clone());
                for android_tool_id in crate::ANDROID_WORKSPACE_TOOL_IDS {
                    preferences
                        .roots
                        .insert(android_tool_id.to_string(), path.clone());
                }
            } else {
                preferences.roots.insert(tool_id.to_string(), path.clone());
            }
            crate::write_tool_root_preferences(data_root, &preferences)?;
            if json {
                print_json(&RootOutput {
                    tool_id,
                    root: Some(&path),
                })
            } else {
                println!("{tool_id} 安装根目录已保存：{}", path.display());
                Ok(())
            }
        }
        _ => Err(AppError::Message("root 只支持 get 或 set".to_string())),
    }
}

async fn install(
    registry: &PluginRegistry,
    data_root: &Path,
    tool_id: &str,
    version: &str,
    confirmed: bool,
    json: bool,
) -> AppResult<()> {
    let plugin = registry.get(tool_id)?;
    let preferences = crate::read_tool_root_preferences(data_root)?;
    let root = preferences.roots.get(tool_id).cloned().ok_or_else(|| {
        AppError::Message(format!(
            "尚未设置 {tool_id} 安装根目录；先运行 {tool_id}-root set <path>"
        ))
    })?;
    let client = http_client()?;
    let catalog = plugin.fetch_available_versions(&client).await?;
    let remote = catalog
        .versions
        .iter()
        .find(|remote| remote.version == version)
        .ok_or_else(|| AppError::Message("官方版本清单中不存在指定版本".to_string()))?;
    let plans = PlanService::new(data_root.to_path_buf());
    let plan = plans.preview_install(plugin.descriptor(), remote, root)?;
    apply_or_preview(registry, data_root, &plans, plan, confirmed, json).await
}

async fn repair(
    registry: &PluginRegistry,
    data_root: &Path,
    tool_id: &str,
    installation_path: PathBuf,
    confirmed: bool,
    json: bool,
) -> AppResult<()> {
    let plugin = registry.get(tool_id)?;
    let client = http_client()?;
    let installer = Installer::new(client.clone(), data_root.to_path_buf());
    let manifest = installer.load_manifest(&installation_path)?;
    if manifest.tool_id != tool_id {
        return Err(AppError::Message(
            "受管安装清单与工具 ID 不匹配".to_string(),
        ));
    }
    let catalog = plugin.fetch_available_versions(&client).await?;
    let remote = catalog
        .versions
        .iter()
        .find(|remote| remote.version == manifest.version)
        .ok_or_else(|| AppError::Message("官方清单中已找不到该版本".to_string()))?;
    let plans = PlanService::new(data_root.to_path_buf());
    let plan = plans.preview_repair(
        plugin.descriptor(),
        remote,
        manifest.managed_root,
        manifest.installation_path,
    )?;
    apply_or_preview(registry, data_root, &plans, plan, confirmed, json).await
}

async fn apply_or_preview(
    registry: &PluginRegistry,
    data_root: &Path,
    plans: &PlanService,
    plan: OperationPlan,
    confirmed: bool,
    json: bool,
) -> AppResult<()> {
    if !confirmed {
        if json {
            print_json(&plan)?;
        } else {
            print_plan(&plan);
            println!();
            println!("未执行。核对以上差异后，在原命令末尾添加 --yes 才会执行。");
        }
        return Ok(());
    }
    if !json {
        print_plan(&plan);
        println!("已收到 --yes，开始执行。");
    }
    let plan = plans.take_confirmed(&plan.id, &plan.confirmation_token)?;
    let client = http_client()?;
    let installer = Installer::new(client, data_root.to_path_buf());
    let result = match &plan.action {
        PlannedAction::Switch { .. }
        | PlannedAction::RestoreEnvironment { .. }
        | PlannedAction::UpdateUserEnvironment { .. } => {
            plans.apply_environment_plan(registry, &plan)?;
            OperationResult {
                operation_id: plan.id.clone(),
                status: "committed".to_string(),
                message: "用户环境已更新；请打开新终端验证，并按需手动重新扫描".to_string(),
                installation_path: None,
            }
        }
        PlannedAction::Uninstall { .. } => {
            let rollback = plans.apply_uninstall_environment(&plan)?;
            match installer.execute_headless(&plan.id, &plan.action).await {
                Ok(result) => result,
                Err(error) => {
                    if let Some(environment) = rollback {
                        plans.rollback_user_environment(&environment)?;
                    }
                    return Err(error);
                }
            }
        }
        PlannedAction::Install(_) | PlannedAction::Repair(_) => {
            installer.execute_headless(&plan.id, &plan.action).await?
        }
        PlannedAction::None => OperationResult {
            operation_id: plan.id.clone(),
            status: "no-op".to_string(),
            message: "计划不包含可执行变更".to_string(),
            installation_path: None,
        },
    };
    if json {
        print_json(&result)
    } else {
        println!("{}：{}", result.status, result.message);
        Ok(())
    }
}

fn print_plan(plan: &OperationPlan) {
    println!("{}", plan.title);
    println!("  {}", plan.summary);
    for warning in &plan.warnings {
        println!("  警告：{warning}");
    }
    for diff in &plan.environment_diffs {
        println!(
            "  环境差异：{:?} {}，新增 {} 项，删除 {} 项",
            diff.scope,
            diff.variable,
            diff.added.len(),
            diff.removed.len()
        );
    }
    for step in &plan.steps {
        println!(
            "  {} {}",
            if step.destructive {
                "[变更]"
            } else {
                "[检查]"
            },
            step.description
        );
    }
}

fn http_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!(
            "EnvNexus-AI-commands/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .https_only(true)
        // 总时限只适用于目录等小请求；安装器下载会按请求覆盖总时限，
        // 由 read_timeout 检测传输中途卡死。
        .timeout(std::time::Duration::from_secs(60))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(AppError::from)
}

fn normalize_tool_id(value: &str) -> &str {
    if value.eq_ignore_ascii_case("jdk") || value.eq_ignore_ascii_case("java") {
        "java"
    } else if value.eq_ignore_ascii_case("nodejs") || value.eq_ignore_ascii_case("node.js") {
        "node"
    } else if value.eq_ignore_ascii_case("android") || value.eq_ignore_ascii_case("sdk") {
        "android-sdk"
    } else if value.eq_ignore_ascii_case("ndk") {
        "android-ndk"
    } else if value.eq_ignore_ascii_case(".net") || value.eq_ignore_ascii_case("dotnet-sdk") {
        "dotnet"
    } else {
        value
    }
}

fn required_arg<'a>(args: &'a [String], index: usize, usage: &str) -> AppResult<&'a str> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| AppError::Message(usage.to_string()))
}

fn remove_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let present = args.iter().any(|argument| argument == flag);
    args.retain(|argument| argument != flag);
    present
}

fn scan_snapshot_path(data_root: &Path) -> PathBuf {
    data_root.join("cache").join("last-environment-scan.json")
}

fn print_json<T: Serialize + ?Sized>(value: &T) -> AppResult<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_help() {
    println!(
        r#"EnvNexus AI 工具命令引擎

用户通常通过命令说明页生成的 *.cmd 命令调用本引擎。只读命令不会自动扫描；
list/diagnose 默认读取桌面 App 保存的上次快照。

  env-tools [--json]
  env-scan [--json]
  EnvNexus-AI.exe command-scripts prepare [--json]
  jdk-list [--json]
  jdk-versions [--json]
  jdk-root get
  jdk-root set <absolute-path>
  jdk-install <version> [--yes] [--json]
  jdk-use <installation-path> [--yes] [--json]
  jdk-repair <installation-path> [--yes] [--json]
  jdk-uninstall <installation-path> [--yes] [--json]

将 jdk 替换为 python、node、go、rust、git、maven、dotnet、ruby、php 等工具前缀。
install/use/repair/uninstall 不带 --yes 时只显示计划，不执行。"#
    );
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDefinitionRow<'a> {
    id: &'a str,
    display_name: &'a str,
    category: &'a str,
    install_root: Option<&'a PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolListOutput<'a> {
    tool_id: &'a str,
    display_name: &'a str,
    scan_finished_at: Option<chrono::DateTime<chrono::Utc>>,
    install_root: Option<&'a PathBuf>,
    inventory: Option<&'a ToolInventory>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootOutput<'a> {
    tool_id: &'a str,
    root: Option<&'a PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_terminal_list_aliases() {
        assert_eq!(normalize_tool_id("jdk"), "java");
        assert_eq!(normalize_tool_id("nodejs"), "node");
        assert_eq!(normalize_tool_id("python"), "python");
    }

    #[test]
    fn removes_confirmation_flag_without_changing_other_arguments() {
        let mut args = vec![
            "install".to_string(),
            "python".to_string(),
            "--yes".to_string(),
        ];
        assert!(remove_flag(&mut args, "--yes"));
        assert_eq!(args, ["install", "python"]);
    }
}
