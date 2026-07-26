use std::path::Path;

use crate::{
    environment::{EnvironmentMap, get_case_insensitive},
    model::{
        DiagnosticCommand, DiagnosticGuidance, DiagnosticIssue, EnvironmentScan, MachineContext,
        ToolRootPreferences,
    },
    terminal,
};

pub fn machine_context(
    data_root: &Path,
    preferences: &ToolRootPreferences,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
) -> MachineContext {
    MachineContext {
        platform: std::env::consts::OS.to_string(),
        process_architecture: std::env::consts::ARCH.to_string(),
        windows_architecture: std::env::var("PROCESSOR_ARCHITECTURE")
            .unwrap_or_else(|_| "unknown".to_string()),
        data_root: data_root.to_path_buf(),
        configured_tool_roots: preferences.roots.clone(),
        user_environment_variable_count: user.len(),
        system_environment_variable_count: system.len(),
    }
}

pub fn guidance_for(
    issue: &DiagnosticIssue,
    scan: &EnvironmentScan,
    machine: &MachineContext,
    user: &EnvironmentMap,
    system: &EnvironmentMap,
) -> DiagnosticGuidance {
    let tool_id = issue_tool_id(&issue.code);
    let tool = tool_id.and_then(|id| scan.tools.iter().find(|tool| tool.id == id));
    let managers = tool_id
        .map(|id| {
            scan.version_managers
                .iter()
                .filter(|manager| manager.tool_ids.iter().any(|tool_id| tool_id == id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut root_causes = vec![issue.detail.clone()];
    let mut machine_factors = Vec::new();
    let mut recommendations = Vec::new();
    let mut commands = Vec::new();
    let mut one_click_available = direct_environment_repair(&issue.code);
    let mut one_click_label = one_click_available.then(|| "预览并一键修复用户环境".to_string());
    let requires_elevation = issue.code.contains("_系统");

    if let Some(evidence) = &issue.evidence {
        root_causes.push(format!("扫描证据：{evidence}"));
    }
    machine_factors.push(format!(
        "Windows 架构={}，EnvNexus AI 进程架构={}；用户变量 {} 个，系统变量 {} 个。",
        machine.windows_architecture,
        machine.process_architecture,
        machine.user_environment_variable_count,
        machine.system_environment_variable_count
    ));
    if let Some(tool_id) = tool_id
        && let Some(root) = machine.configured_tool_roots.get(tool_id)
    {
        machine_factors.push(format!(
            "{} 的 EnvNexus AI 安装根目录已设置为 {}。",
            display_tool(tool_id),
            root.display()
        ));
    }
    if let Some(tool) = tool {
        machine_factors.push(format!(
            "上次扫描发现 {} 个版本；当前默认={}。",
            tool.installed_versions.len(),
            tool.default_version
                .as_ref()
                .map(|version| format!("{} ({})", version.version, version.path.display()))
                .unwrap_or_else(|| "未解析到".to_string())
        ));
    }
    if !managers.is_empty() {
        machine_factors.push(format!(
            "已检测版本管理器：{}。其 shim、符号链接和根目录必须优先保留。",
            managers
                .iter()
                .map(|manager| manager.display_name.as_str())
                .collect::<Vec<_>>()
                .join("、")
        ));
        for manager in &managers {
            if let Some(command) = manager_inspection_command(&manager.id) {
                commands.push(read_command(
                    &format!("查看 {} 状态", manager.display_name),
                    command,
                ));
            }
        }
    }

    match issue.code.as_str() {
        code if code.starts_with("PATH_DUPLICATE_用户") => {
            recommendations.extend([
                "保留第一次出现的路径，删除后续完全相同的用户 PATH 条目。".to_string(),
                "不要删除版本管理器的 shim、current 或 symlink 目录。".to_string(),
                "执行后打开新终端并重新扫描，确认命令解析顺序。".to_string(),
            ]);
        }
        code if code.starts_with("PATH_MISSING_用户") => {
            recommendations.extend([
                "只删除当前确实不存在、且不属于离线磁盘或临时网络盘的用户 PATH 条目。".to_string(),
                "若路径所在盘符当前未挂载，应保留并先恢复磁盘。".to_string(),
            ]);
        }
        code if code.starts_with("PATH_EMPTY_用户") || code.starts_with("PATH_RELATIVE_用户") =>
        {
            recommendations.extend([
                "删除空条目、相对条目或被分号截断的条目，避免隐式搜索当前目录。".to_string(),
                "在差异预览中逐项核对，确认不是原路径中未转义的分号。".to_string(),
            ]);
        }
        code if code.starts_with("PATH_") && code.ends_with("_系统") => {
            one_click_available = false;
            one_click_label = None;
            recommendations.extend([
                "EnvNexus AI 不自动修改系统 PATH；先确认条目属于哪个安装器或组织策略。".to_string(),
                "需要修改时使用管理员界面或原厂安装器，并提前创建系统还原点。".to_string(),
            ]);
        }
        code if code.starts_with("ENV_DUPLICATE_SCOPE_") => {
            let name = code.trim_start_matches("ENV_DUPLICATE_SCOPE_");
            let user_value = get_case_insensitive(user, name);
            let system_value = get_case_insensitive(system, name);
            commands.push(read_command(
                &format!("读取 {name} 两个作用域"),
                &format!(
                    "[Environment]::GetEnvironmentVariable('{name}','User'); [Environment]::GetEnvironmentVariable('{name}','Machine')"
                ),
            ));
            if user_value.is_some()
                && system_value.is_some()
                && user_value.is_some_and(|left| {
                    system_value.is_some_and(|right| left.eq_ignore_ascii_case(right))
                })
            {
                recommendations
                    .push("两个值相同，可只删除重复的用户变量；系统值保持不变。".to_string());
            } else {
                one_click_available = false;
                one_click_label = None;
                root_causes
                    .push("两个作用域的值不同，无法仅凭变量名判断应保留哪一套工具链。".to_string());
                recommendations.push(
                    "先用版本命令验证两个目录，再选择保留版本；不要自动删除任一作用域。"
                        .to_string(),
                );
            }
        }
        code if code.starts_with("TOOL_ENV_ALIAS_CONFLICT_") => {
            one_click_available = false;
            one_click_label = None;
            let token = code.trim_start_matches("TOOL_ENV_ALIAS_CONFLICT_");
            root_causes.push(
                "自定义变量名不会自动决定 PATH 优先级，但常被脚本间接引用，可能造成“终端正常、IDE 异常”的差异。"
                    .to_string(),
            );
            recommendations.extend([
                "逐个确认变量被哪些 PowerShell 配置、批处理、IDE 或构建脚本引用。".to_string(),
                "保留版本管理器要求的标准变量；自定义别名先停用引用，再考虑删除。".to_string(),
                "不要把 RUST1/RUST2 一类变量直接改名覆盖 CARGO_HOME 或 RUSTUP_HOME。".to_string(),
            ]);
            commands.push(read_command(
                "列出相关自定义环境变量",
                &format!(
                    "Get-ChildItem Env: | Where-Object {{ $_.Name -match '(?i){}' }} | Sort-Object Name",
                    alias_pattern(token)
                ),
            ));
        }
        code if code.starts_with("MULTIPLE_VERSION_MANAGERS_") => {
            one_click_available = false;
            one_click_label = None;
            root_causes.push(
                "多个版本管理器可能同时把 shim 或链接目录放入 PATH，先出现者会截获命令。"
                    .to_string(),
            );
            recommendations.extend([
                "为该工具只选择一个主要版本管理器，其余管理器先从 shell 初始化脚本中停用。"
                    .to_string(),
                "核对 CMD、PowerShell、IDE 集成终端的初始化文件，避免只修复一种终端。".to_string(),
            ]);
        }
        code if code.ends_with("_PATH_SHADOWING") => {
            one_click_available = false;
            one_click_label = None;
            recommendations.extend([
                "先确认 PATH 中第一个命令是否为期望版本或版本管理器 shim。".to_string(),
                "若使用版本管理器，应保留其 shim 在普通安装目录之前。".to_string(),
                "只有确定要由 EnvNexus AI 接管时，才切换默认版本。".to_string(),
            ]);
        }
        code if code.ends_with("_NO_DEFAULT") => {
            if tool.is_some_and(|tool| tool.installed_versions.len() == 1) {
                one_click_available = true;
                one_click_label = Some("预览切换到唯一已发现版本".to_string());
                recommendations.push(
                    "只发现一个版本，可生成用户级 PATH/变量切换计划；仍需确认后执行。".to_string(),
                );
                if let (Some(tool_id), Some(version)) = (
                    tool_id,
                    tool.and_then(|tool| tool.installed_versions.first()),
                ) {
                    commands.extend(switch_commands(
                        terminal::command_prefix(tool_id),
                        &version.path,
                    ));
                }
            } else {
                one_click_available = false;
                one_click_label = None;
                recommendations.push(
                    "先在工具详情中选择目标版本；多个版本并存时不能自动猜测默认版本。".to_string(),
                );
            }
        }
        "JAVA_HOME_DEFAULT_MISMATCH" => {
            recommendations.push(
                "以项目和构建工具实际需要的 JDK 为准，让 JAVA_HOME 与 PATH 中第一个 java.exe 对齐。"
                    .to_string(),
            );
            one_click_available = tool
                .and_then(|tool| tool.default_version.as_ref())
                .is_some();
            one_click_label =
                one_click_available.then(|| "预览对齐 JAVA_HOME 与默认 Java".to_string());
            if let Some(version) = tool.and_then(|tool| tool.default_version.as_ref()) {
                commands.extend(switch_commands("jdk", &version.path));
            }
        }
        _ => {
            recommendations.extend([
                "先执行只读命令确认现状，再在工具详情中选择目标版本。".to_string(),
                "无法唯一判断的项目不会提供自动写入；重新扫描可验证处理结果。".to_string(),
            ]);
        }
    }

    if let Some(tool_id) = tool_id {
        let prefix = terminal::command_prefix(tool_id);
        let executable = executable_name(tool_id);
        commands.extend([
            read_command(
                &format!("查看所有 {executable} 命令来源"),
                &format!("where.exe {executable}"),
            ),
            read_command(
                &format!("查看 {} 已安装版本", display_tool(tool_id)),
                &format!("{prefix}-list"),
            ),
            read_command(
                &format!("查看 {} 管理目录", display_tool(tool_id)),
                &format!("{prefix}-root get"),
            ),
        ]);
    }
    if one_click_available && direct_environment_repair(&issue.code) {
        commands.push(DiagnosticCommand {
            label: "预览本地修复计划".to_string(),
            shell: "CMD / PowerShell".to_string(),
            command: format!("env-repair \"{}\"", issue.code),
            changes_environment: false,
            requires_elevation: false,
        });
        commands.push(DiagnosticCommand {
            label: "确认执行并自动备份".to_string(),
            shell: "CMD / PowerShell".to_string(),
            command: format!("env-repair \"{}\" --yes", issue.code),
            changes_environment: true,
            requires_elevation: false,
        });
    }
    commands.push(read_command("修复后重新扫描", "env-scan"));

    DiagnosticGuidance {
        issue_code: issue.code.clone(),
        analysis_source: "EnvNexus AI 本地规则引擎".to_string(),
        summary: issue.title.clone(),
        root_causes,
        machine_factors,
        recommendations,
        commands,
        one_click_available,
        one_click_label,
        requires_elevation,
    }
}

pub fn issue_tool_id(code: &str) -> Option<&'static str> {
    // 工具级诊断码由 descriptor.id 大写生成，含连字符（如 ANDROID-NDK_NO_DEFAULT）；
    // 统一为下划线后再匹配，避免 android-ndk 误落到 ANDROID 前缀。
    let code = code.replace('-', "_");
    let code = code.as_str();
    [
        ("ANDROID_SDK", "android-sdk"),
        ("ANDROID_NDK", "android-ndk"),
        ("ANDROID", "android-sdk"),
        ("PYTHON", "python"),
        ("GRADLE", "gradle"),
        ("NODE", "node"),
        ("MAVEN", "maven"),
        ("DOTNET", "dotnet"),
        ("RUBY", "ruby"),
        ("PHP", "php"),
        ("CMAKE", "cmake"),
        ("JAVA", "java"),
        ("JDK", "java"),
        ("RUST", "rust"),
        ("CARGO", "rust"),
        ("GIT", "git"),
        ("ADB", "adb"),
        ("GO", "go"),
    ]
    .into_iter()
    .find_map(|(prefix, tool_id)| code.contains(prefix).then_some(tool_id))
}

fn direct_environment_repair(code: &str) -> bool {
    code.starts_with("PATH_DUPLICATE_用户")
        || code.starts_with("PATH_MISSING_用户")
        || code.starts_with("PATH_EMPTY_用户")
        || code.starts_with("PATH_RELATIVE_用户")
        || code.starts_with("ENV_DUPLICATE_SCOPE_")
}

fn read_command(label: &str, command: &str) -> DiagnosticCommand {
    DiagnosticCommand {
        label: label.to_string(),
        shell: "PowerShell".to_string(),
        command: command.to_string(),
        changes_environment: false,
        requires_elevation: false,
    }
}

fn switch_commands(prefix: &str, path: &Path) -> [DiagnosticCommand; 2] {
    let path = path.to_string_lossy();
    [
        DiagnosticCommand {
            label: "预览默认版本切换".to_string(),
            shell: "CMD / PowerShell".to_string(),
            command: format!("{prefix}-use \"{path}\""),
            changes_environment: false,
            requires_elevation: false,
        },
        DiagnosticCommand {
            label: "确认切换并自动备份".to_string(),
            shell: "CMD / PowerShell".to_string(),
            command: format!("{prefix}-use \"{path}\" --yes"),
            changes_environment: true,
            requires_elevation: false,
        },
    ]
}

fn manager_inspection_command(manager_id: &str) -> Option<&'static str> {
    match manager_id {
        "pyenv-win" => Some("pyenv versions"),
        "nvm-windows" => Some("nvm list"),
        "fnm" => Some("fnm list"),
        "volta" => Some("volta list"),
        "rustup" => Some("rustup show; rustup toolchain list"),
        "jabba" => Some("jabba ls"),
        "goenv" => Some("goenv versions"),
        "rbenv" => Some("rbenv versions"),
        "uru" => Some("uru ls"),
        _ => None,
    }
}

fn alias_pattern(token: &str) -> &'static str {
    match token {
        "PYTHON" => "PYTHON|PYENV",
        "JAVA" => "JAVA|JDK|JABBA",
        "GO" => "GOENV|GOROOT|GOPATH|^GO[0-9_]",
        "RUST" => "RUST|CARGO",
        "NODE" => "NODE|NVM|FNM|VOLTA",
        "DOTNET" => "DOTNET|\\.NET",
        "ANDROID" => "ANDROID|SDK|NDK",
        "RUBY" => "RUBY|RBENV|URU",
        "MAVEN" => "MAVEN|M2",
        _ => "PYTHON|JAVA|JDK|GO|RUST|CARGO|NODE|NVM|DOTNET|ANDROID|RUBY|MAVEN|PHP",
    }
}

fn display_tool(tool_id: &str) -> &'static str {
    match tool_id {
        "python" => "Python",
        "java" => "Java / JDK",
        "go" => "Go",
        "rust" => "Rust",
        "node" => "Node.js",
        "git" => "Git",
        "android-sdk" => "Android SDK",
        "android-ndk" => "Android NDK",
        "gradle" => "Gradle",
        "cmake" => "CMake",
        "adb" => "ADB",
        "maven" => "Maven",
        "dotnet" => ".NET SDK",
        "ruby" => "Ruby",
        "php" => "PHP",
        _ => "开发工具",
    }
}

fn executable_name(tool_id: &str) -> &'static str {
    match tool_id {
        "python" => "python.exe",
        "java" => "java.exe",
        "go" => "go.exe",
        "rust" => "rustc.exe",
        "node" => "node.exe",
        "git" => "git.exe",
        "android-sdk" => "sdkmanager.bat",
        "android-ndk" => "ndk-build.cmd",
        "gradle" => "gradle.bat",
        "cmake" => "cmake.exe",
        "adb" => "adb.exe",
        "maven" => "mvn.cmd",
        "dotnet" => "dotnet.exe",
        "ruby" => "ruby.exe",
        "php" => "php.exe",
        _ => "tool.exe",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EnvironmentScan, IssueLevel};
    use chrono::Utc;

    #[test]
    fn custom_alias_conflict_has_local_commands_but_no_automatic_write() {
        let issue = DiagnosticIssue {
            code: "TOOL_ENV_ALIAS_CONFLICT_RUST".to_string(),
            level: IssueLevel::Warning,
            title: "Rust 自定义环境变量指向多个目录".to_string(),
            detail: "发现 RUST1/RUST2".to_string(),
            evidence: Some("用户 RUST1=E:\\Rust1 | 用户 RUST2=E:\\Rust2".to_string()),
            repairable: false,
        };
        let scan = EnvironmentScan {
            tools: Vec::new(),
            issues: vec![issue.clone()],
            version_managers: Vec::new(),
            user_path_entries: 0,
            scan_started_at: Utc::now(),
            scan_finished_at: Utc::now(),
        };
        let machine = MachineContext {
            platform: "windows".to_string(),
            process_architecture: "x86_64".to_string(),
            windows_architecture: "AMD64".to_string(),
            data_root: "E:\\EnvNexus AI".into(),
            configured_tool_roots: Default::default(),
            user_environment_variable_count: 2,
            system_environment_variable_count: 0,
        };
        let guidance = guidance_for(
            &issue,
            &scan,
            &machine,
            &EnvironmentMap::new(),
            &EnvironmentMap::new(),
        );
        assert!(!guidance.one_click_available);
        assert!(
            guidance
                .commands
                .iter()
                .any(|command| command.command.contains("Get-ChildItem Env:"))
        );
    }
}
