use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, AppResult},
    model::{DiagnosticIssue, EnvironmentScope, IssueLevel},
};

#[cfg(windows)]
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ},
};

pub type EnvironmentMap = BTreeMap<String, String>;

#[cfg(windows)]
pub fn read_environment(scope: EnvironmentScope) -> AppResult<EnvironmentMap> {
    let (root, path) = match scope {
        EnvironmentScope::User => (RegKey::predef(HKEY_CURRENT_USER), "Environment"),
        EnvironmentScope::System => (
            RegKey::predef(HKEY_LOCAL_MACHINE),
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
    };
    let key = root
        .open_subkey_with_flags(path, KEY_READ)
        .map_err(|error| AppError::Message(format!("读取 Windows 环境变量失败：{error}")))?;
    let mut values = EnvironmentMap::new();
    for entry in key.enum_values() {
        let Ok((name, _)) = entry else {
            continue;
        };
        if let Ok(value) = key.get_value::<String, _>(&name) {
            values.insert(name, value);
        }
    }
    Ok(values)
}

#[cfg(not(windows))]
pub fn read_environment(_scope: EnvironmentScope) -> AppResult<EnvironmentMap> {
    Ok(std::env::vars().collect())
}

pub fn get_case_insensitive<'a>(map: &'a EnvironmentMap, name: &str) -> Option<&'a String> {
    map.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

pub fn split_path(value: Option<&String>) -> Vec<String> {
    value
        .map(|path| {
            path.split(';')
                .map(|entry| entry.trim().trim_matches('"').to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub fn environment_fingerprint(map: &EnvironmentMap) -> String {
    let mut hasher = Sha256::new();
    for (key, value) in map {
        hasher.update(key.to_ascii_uppercase().as_bytes());
        hasher.update([0]);
        hasher.update(value.as_bytes());
        hasher.update([0xff]);
    }
    hex::encode(hasher.finalize())
}

pub fn merged_value<'a>(
    user: &'a EnvironmentMap,
    system: &'a EnvironmentMap,
    name: &str,
) -> Vec<(&'static str, &'a str)> {
    let mut values = Vec::new();
    if let Some(value) = get_case_insensitive(user, name) {
        values.push(("用户", value.as_str()));
    }
    if let Some(value) = get_case_insensitive(system, name) {
        values.push(("系统", value.as_str()));
    }
    values
}

pub fn diagnose_environment(
    user: &EnvironmentMap,
    system: &EnvironmentMap,
) -> Vec<DiagnosticIssue> {
    let mut issues = Vec::new();
    diagnose_path_scope(user, EnvironmentScope::User, &mut issues);
    diagnose_path_scope(system, EnvironmentScope::System, &mut issues);

    for name in [
        "JAVA_HOME",
        "GOROOT",
        "PYENV",
        "PYENV_ROOT",
        "NVM_HOME",
        "NVM_SYMLINK",
        "ANDROID_HOME",
        "ANDROID_SDK_ROOT",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "MAVEN_HOME",
        "M2_HOME",
        "DOTNET_ROOT",
        "DOTNET_ROOT_X64",
        "RUBY_HOME",
        "RBENV_ROOT",
        "URU_HOME",
        "PHP_HOME",
    ] {
        let Some(user_value) = get_case_insensitive(user, name) else {
            continue;
        };
        let Some(system_value) = get_case_insensitive(system, name) else {
            continue;
        };
        issues.push(DiagnosticIssue {
            code: format!("ENV_DUPLICATE_SCOPE_{}", name.to_ascii_uppercase()),
            level: IssueLevel::Warning,
            title: format!("{name} 同时存在于用户级和系统级"),
            detail: if user_value.eq_ignore_ascii_case(system_value) {
                "两个作用域的值相同，虽然能工作，但会让后续切换和恢复的归属不明确。".to_string()
            } else {
                "两个作用域的值不同，实际结果取决于 Windows 合并和进程启动方式。".to_string()
            },
            evidence: Some(format!("用户: {user_value} | 系统: {system_value}")),
            repairable: user_value.eq_ignore_ascii_case(system_value),
        });
    }

    if let (Some(home), Some(root)) = (
        get_case_insensitive(user, "ANDROID_HOME")
            .or_else(|| get_case_insensitive(system, "ANDROID_HOME")),
        get_case_insensitive(user, "ANDROID_SDK_ROOT")
            .or_else(|| get_case_insensitive(system, "ANDROID_SDK_ROOT")),
    ) && !home.eq_ignore_ascii_case(root)
    {
        issues.push(DiagnosticIssue {
            code: "ANDROID_ROOT_CONFLICT".to_string(),
            level: IssueLevel::Error,
            title: "Android SDK 根目录变量冲突".to_string(),
            detail: "ANDROID_HOME 与 ANDROID_SDK_ROOT 指向不同位置。".to_string(),
            evidence: Some(format!("ANDROID_HOME={home} | ANDROID_SDK_ROOT={root}")),
            repairable: false,
        });
    }

    diagnose_semantic_variable_pair(
        user,
        system,
        "MAVEN_HOME",
        "M2_HOME",
        "MAVEN_HOME_M2_HOME_CONFLICT",
        "Maven 根目录变量冲突",
        &mut issues,
    );
    diagnose_tool_alias_variables(user, system, &mut issues);

    issues
}

fn diagnose_semantic_variable_pair(
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    left_name: &str,
    right_name: &str,
    code: &str,
    title: &str,
    issues: &mut Vec<DiagnosticIssue>,
) {
    let left =
        get_case_insensitive(user, left_name).or_else(|| get_case_insensitive(system, left_name));
    let right =
        get_case_insensitive(user, right_name).or_else(|| get_case_insensitive(system, right_name));
    if let (Some(left), Some(right)) = (left, right)
        && normalize_path_key(left) != normalize_path_key(right)
    {
        issues.push(DiagnosticIssue {
            code: code.to_string(),
            level: IssueLevel::Warning,
            title: title.to_string(),
            detail: format!("{left_name} 与 {right_name} 应描述同一套工具，但当前指向不同目录。"),
            evidence: Some(format!("{left_name}={left} | {right_name}={right}")),
            repairable: false,
        });
    }
}

fn diagnose_tool_alias_variables(
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    issues: &mut Vec<DiagnosticIssue>,
) {
    let canonical = [
        "JAVA_HOME",
        "GOROOT",
        "GOPATH",
        "PYENV",
        "PYENV_ROOT",
        "NVM_HOME",
        "NVM_SYMLINK",
        "FNM_DIR",
        "FNM_MULTISHELL_PATH",
        "VOLTA_HOME",
        "ANDROID_HOME",
        "ANDROID_SDK_ROOT",
        "ANDROID_NDK_HOME",
        "ANDROID_NDK_ROOT",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "MAVEN_HOME",
        "M2_HOME",
        "DOTNET_ROOT",
        "DOTNET_ROOT_X64",
        "RUBY_HOME",
        "RBENV_ROOT",
        "URU_HOME",
        "PHP_HOME",
        "PATH",
    ];
    let mut by_tool = HashMap::<&'static str, Vec<String>>::new();
    for (scope, environment) in [("用户", user), ("系统", system)] {
        for (name, value) in environment {
            let upper = name.to_ascii_uppercase();
            if canonical.iter().any(|known| upper == *known) || !looks_like_absolute_path(value) {
                continue;
            }
            let Some(tool) = variable_tool_token(&upper) else {
                continue;
            };
            by_tool
                .entry(tool)
                .or_default()
                .push(format!("{scope} {name}={value}"));
        }
    }
    for (tool, entries) in by_tool {
        let distinct = entries
            .iter()
            .filter_map(|entry| entry.split_once('=').map(|(_, value)| value))
            .map(normalize_path_key)
            .collect::<HashSet<_>>();
        if entries.len() < 2 || distinct.len() < 2 {
            continue;
        }
        issues.push(DiagnosticIssue {
            code: format!("TOOL_ENV_ALIAS_CONFLICT_{tool}"),
            level: IssueLevel::Warning,
            title: format!("{} 自定义环境变量指向多个目录", tool_display_name(tool)),
            detail:
                "这些非标准变量可能被 PowerShell 配置、批处理、IDE 或构建脚本间接引用；仅查看 PATH 无法确定实际使用版本。"
                    .to_string(),
            evidence: Some(limit_evidence(&entries)),
            repairable: false,
        });
    }
}

fn looks_like_absolute_path(value: &str) -> bool {
    let value = value.trim().trim_matches('"');
    !value.contains(';') && Path::new(value).is_absolute()
}

fn variable_tool_token(name: &str) -> Option<&'static str> {
    if name.contains("PYTHON") || name.contains("PYENV") {
        Some("PYTHON")
    } else if name.contains("JAVA") || name.contains("JDK") || name.contains("JABBA") {
        Some("JAVA")
    } else if name.contains("RUST") || name.contains("CARGO") {
        Some("RUST")
    } else if name.contains("NODE")
        || name.contains("NVM")
        || name.contains("FNM")
        || name.contains("VOLTA")
    {
        Some("NODE")
    } else if name.contains("DOTNET") {
        Some("DOTNET")
    } else if name.contains("ANDROID") || name.contains("NDK") {
        Some("ANDROID")
    } else if name.contains("MAVEN") || name.starts_with("M2") {
        Some("MAVEN")
    } else if name.contains("RUBY") || name.contains("RBENV") || name.contains("URU") {
        Some("RUBY")
    } else if name.contains("PHP") {
        Some("PHP")
    } else if name.starts_with("GO") && !name.contains("OGLE") {
        Some("GO")
    } else {
        None
    }
}

fn tool_display_name(tool: &str) -> &'static str {
    match tool {
        "PYTHON" => "Python",
        "JAVA" => "Java / JDK",
        "GO" => "Go",
        "RUST" => "Rust",
        "NODE" => "Node.js",
        "DOTNET" => ".NET",
        "ANDROID" => "Android",
        "MAVEN" => "Maven",
        "RUBY" => "Ruby",
        "PHP" => "PHP",
        _ => "开发工具",
    }
}

fn diagnose_path_scope(
    environment: &EnvironmentMap,
    scope: EnvironmentScope,
    issues: &mut Vec<DiagnosticIssue>,
) {
    let Some(path_value) = get_case_insensitive(environment, "PATH") else {
        return;
    };
    let scope_name = match scope {
        EnvironmentScope::User => "用户",
        EnvironmentScope::System => "系统",
    };
    let entries = split_path(Some(path_value));
    let mut seen = HashMap::<String, usize>::new();
    let mut duplicates = Vec::new();
    let mut invalid = Vec::new();
    let mut relative = Vec::new();
    let mut empty_count = 0usize;

    for (index, entry) in entries.iter().enumerate() {
        if entry.is_empty() {
            empty_count += 1;
            continue;
        }
        let normalized = normalize_path_key(entry);
        if let Some(previous) = seen.insert(normalized, index) {
            duplicates.push(format!("#{} 与 #{}: {}", previous + 1, index + 1, entry));
        }

        if entry.contains('%') {
            continue;
        }
        let path = Path::new(entry);
        if !path.is_absolute() {
            relative.push(format!("#{}: {}", index + 1, entry));
        } else if !path.exists() {
            invalid.push(format!("#{}: {}", index + 1, entry));
        }
    }

    if !duplicates.is_empty() {
        issues.push(DiagnosticIssue {
            code: format!("PATH_DUPLICATE_{scope_name}"),
            level: IssueLevel::Warning,
            title: format!("{scope_name} PATH 包含重复条目"),
            detail: format!(
                "发现 {} 组重复路径，会增加切换结果的不确定性。",
                duplicates.len()
            ),
            evidence: Some(limit_evidence(&duplicates)),
            repairable: scope == EnvironmentScope::User,
        });
    }
    if !relative.is_empty() {
        issues.push(DiagnosticIssue {
            code: format!("PATH_RELATIVE_{scope_name}"),
            level: IssueLevel::Error,
            title: format!("{scope_name} PATH 包含相对或被截断的条目"),
            detail: "PATH 条目应为绝对路径；相对路径也可能来自路径中未正确处理的分号。".to_string(),
            evidence: Some(limit_evidence(&relative)),
            repairable: scope == EnvironmentScope::User,
        });
    }
    if !invalid.is_empty() {
        issues.push(DiagnosticIssue {
            code: format!("PATH_MISSING_{scope_name}"),
            level: IssueLevel::Warning,
            title: format!("{scope_name} PATH 包含不存在的目录"),
            detail: format!("发现 {} 个当前不存在的绝对路径。", invalid.len()),
            evidence: Some(limit_evidence(&invalid)),
            repairable: scope == EnvironmentScope::User,
        });
    }
    if empty_count > 0 {
        issues.push(DiagnosticIssue {
            code: format!("PATH_EMPTY_{scope_name}"),
            level: IssueLevel::Warning,
            title: format!("{scope_name} PATH 包含空条目"),
            detail: format!("发现 {empty_count} 个空条目；空 PATH 条目可能隐式引用当前目录。"),
            evidence: None,
            repairable: scope == EnvironmentScope::User,
        });
    }
}

fn normalize_path_key(value: &str) -> String {
    let trimmed = value.trim_end_matches(['\\', '/']);
    trimmed.replace('/', "\\").to_ascii_lowercase()
}

fn limit_evidence(items: &[String]) -> String {
    let mut output = items
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(" | ");
    if items.len() > 5 {
        output.push_str(&format!(" | 另有 {} 项", items.len() - 5));
    }
    output
}

pub fn collect_candidate_roots(
    user: &EnvironmentMap,
    system: &EnvironmentMap,
    names: &[&str],
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for name in names {
        for (_, value) in merged_value(user, system, name) {
            let path = PathBuf::from(value.trim_matches('"'));
            let key = normalize_path_key(&path.to_string_lossy());
            if seen.insert(key) {
                roots.push(path);
            }
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_duplicate_relative_missing_and_empty_path_entries() {
        let mut user = EnvironmentMap::new();
        let missing = std::env::temp_dir().join("envpilot-path-that-does-not-exist");
        user.insert(
            "Path".into(),
            format!(
                "C:\\Windows;C:\\Windows;relative\\bin;{};;",
                missing.display()
            ),
        );
        let issues = diagnose_environment(&user, &EnvironmentMap::new());
        assert!(
            issues
                .iter()
                .any(|issue| issue.code.starts_with("PATH_DUPLICATE"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.code.starts_with("PATH_RELATIVE"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.code.starts_with("PATH_MISSING"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.code.starts_with("PATH_EMPTY"))
        );
    }

    #[test]
    fn fingerprint_is_order_stable_for_btree_map() {
        let mut values = EnvironmentMap::new();
        values.insert("Path".into(), "A;B".into());
        let first = environment_fingerprint(&values);
        let second = environment_fingerprint(&values);
        assert_eq!(first, second);
    }

    #[test]
    fn detects_custom_rust_aliases_that_point_to_different_versions() {
        let mut user = EnvironmentMap::new();
        user.insert("RUST1".to_string(), r"E:\Rust\1.80".to_string());
        user.insert("RUST2".to_string(), r"E:\Rust\1.90".to_string());

        let issues = diagnose_environment(&user, &EnvironmentMap::new());

        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "TOOL_ENV_ALIAS_CONFLICT_RUST")
        );
    }
}
