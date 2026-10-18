use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    environment::{EnvironmentMap, get_case_insensitive, split_path},
    error::{AppError, AppResult},
    process::{output_text, run_capture_sanitized},
};

#[derive(Debug, Clone)]
pub(crate) struct MavenJavaRuntime {
    pub home: PathBuf,
    pub version: String,
    pub source: String,
    pub notes: Vec<String>,
}

impl MavenJavaRuntime {
    pub fn plan_warning(&self) -> String {
        let details = if self.notes.is_empty() {
            String::new()
        } else {
            format!("{}。", self.notes.join("；"))
        };
        format!(
            "{details}安装验证将临时使用 JDK {}（{}），JAVA_HOME={}。这只作用于 Maven 验证子进程；终端默认 Java 仍需在 Java 页面另行配置。",
            self.version,
            self.source,
            self.home.display()
        )
    }
}

pub(crate) fn maven_minimum_java(version: &str) -> u32 {
    // Maven 4 alpha-13 raised the baseline; earlier alphas still ran on Java 8.
    // https://maven.apache.org/docs/history.html
    if version
        .strip_prefix("4.0.0-alpha-")
        .and_then(|value| value.parse::<u32>().ok())
        .is_some_and(|alpha| alpha < 13)
    {
        return 8;
    }
    if version
        .split('.')
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .is_some_and(|major| major >= 4)
    {
        17
    } else {
        8
    }
}

/// Resolve and execute the actual JDK tools before downloading Maven. Never
/// modify the parent process or registry when repairing a stale JAVA_HOME.
pub(crate) fn resolve_maven_java(
    data_root: &Path,
    managed_root: Option<&Path>,
    version: &str,
    current: &EnvironmentMap,
) -> AppResult<MavenJavaRuntime> {
    let preferences = crate::read_tool_root_preferences(data_root)?;
    let mut roots = preferences
        .roots
        .get("java")
        .cloned()
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(root) = managed_root {
        roots.push(root.to_path_buf());
    }
    let mut homes = crate::installer::managed_manifests(data_root)
        .into_iter()
        .filter(|manifest| manifest.tool_id == "java")
        .map(|manifest| manifest.installation_path)
        .collect::<Vec<_>>();
    for root in roots {
        homes.push(root.clone());
        for versions in [root.clone(), root.join("java")] {
            if let Ok(children) = fs::read_dir(versions) {
                homes.extend(
                    children
                        .take(512)
                        .filter_map(Result::ok)
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                        .map(|entry| entry.path()),
                );
            }
        }
    }
    homes.sort_by(|left, right| {
        crate::versioning::compare_version_strings(
            &java_release_version(right),
            &java_release_version(left),
        )
        .then_with(|| left.cmp(right))
    });
    let inherited = ["JAVA_HOME", "PATH"]
        .into_iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_string(), value))
        })
        .collect::<EnvironmentMap>();
    resolve_maven_java_candidates_with_process(version, current, Some(&inherited), &homes, |home| {
        probe_jdk(home, current, data_root)
    })
}

fn java_release_version(home: &Path) -> String {
    fs::read_to_string(home.join("release"))
        .ok()
        .and_then(|content| {
            content.lines().find_map(|line| {
                line.strip_prefix("JAVA_VERSION=")
                    .map(|value| value.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| {
            home.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
}

#[cfg(test)]
fn resolve_maven_java_candidates<F>(
    version: &str,
    current: &EnvironmentMap,
    managed: &[PathBuf],
    probe: F,
) -> AppResult<MavenJavaRuntime>
where
    F: FnMut(&Path) -> AppResult<(String, u32, u32)>,
{
    resolve_maven_java_candidates_with_process(version, current, None, managed, probe)
}

fn resolve_maven_java_candidates_with_process<F>(
    version: &str,
    current: &EnvironmentMap,
    inherited: Option<&EnvironmentMap>,
    managed: &[PathBuf],
    mut probe: F,
) -> AppResult<MavenJavaRuntime>
where
    F: FnMut(&Path) -> AppResult<(String, u32, u32)>,
{
    let minimum = maven_minimum_java(version);
    let mut candidates = Vec::new();
    let mut notes = Vec::new();
    for (environment, label) in std::iter::once((current, "最新"))
        .chain(inherited.map(|environment| (environment, "当前进程")))
    {
        if let Some(raw) =
            get_case_insensitive(environment, "JAVA_HOME").filter(|value| !value.trim().is_empty())
        {
            let value = raw.trim();
            let unquoted = value.trim_matches('"');
            let mut home = PathBuf::from(unquoted);
            if value != unquoted {
                notes.push(format!("{label} JAVA_HOME 含多余引号"));
            }
            if home
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("bin"))
            {
                notes.push(format!("{label} JAVA_HOME 指向 bin，正确值应为 JDK 根目录"));
                if let Some(parent) = home.parent() {
                    home = parent.to_path_buf();
                }
            }
            candidates.push((home, format!("{label} JAVA_HOME")));
        }
        let path = get_case_insensitive(environment, "PATH");
        for entry in split_path(path)
            .into_iter()
            .filter(|entry| !entry.is_empty() && !entry.contains('%'))
        {
            let executable = PathBuf::from(entry).join("java.exe");
            let Ok(executable) = crate::paths::canonicalize_simplified(&executable) else {
                continue;
            };
            let Some(bin) = executable.parent().filter(|directory| {
                directory
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("bin"))
            }) else {
                continue;
            };
            if let Some(home) = bin.parent() {
                candidates.push((home.to_path_buf(), format!("{label} PATH 中的 JDK")));
            }
        }
    }
    candidates.extend(
        managed
            .iter()
            .cloned()
            .map(|home| (home, "已配置或受管 JDK".to_string())),
    );
    let mut seen = HashSet::new();
    let mut failures = Vec::new();
    for (home, source) in candidates {
        if !home.is_absolute() {
            continue;
        }
        let key = home
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        if !home.join("bin/java.exe").is_file() || !home.join("bin/javac.exe").is_file() {
            if source.contains("JAVA_HOME") {
                notes.push(format!(
                    "JAVA_HOME={} 缺少 bin/java.exe 或 bin/javac.exe",
                    home.display()
                ));
            }
            continue;
        }
        match probe(&home) {
            Ok((java_version,java_major,javac_major)) if java_major>=minimum && javac_major>=minimum && java_major==javac_major => {
                if !failures.is_empty(){notes.push(failures.join("；"));}
                return Ok(MavenJavaRuntime{home,version:java_version,source,notes});
            }
            Ok((java_version,java_major,javac_major))=>failures.push(format!("{} 的 Java {java_version} / javac {javac_major} 不满足 JDK {minimum}+ 或版本不一致（Java 主版本 {java_major}）",home.display())),
            Err(error)=>failures.push(format!("{} 的 JDK 命令不能正常执行：{error}",home.display())),
        }
        failures.truncate(3);
    }
    notes.extend(failures);
    let detail = if notes.is_empty() {
        "当前 JAVA_HOME、PATH 和受管 Java 目录中均没有可用的完整 JDK".to_string()
    } else {
        notes.join("；")
    };
    Err(AppError::Message(format!(
        "Maven {version} 需要完整 JDK {minimum}+（java.exe 和 javac.exe 均须可运行）。{detail}。请先在 Java 页面安装或配置 JDK，再安装或修复 Maven；这里不会自动切换用户的默认 Java。"
    )))
}

fn probe_jdk(
    home: &Path,
    current: &EnvironmentMap,
    data_root: &Path,
) -> AppResult<(String, u32, u32)> {
    let environment = maven_verification_environment(
        current,
        &MavenJavaRuntime {
            home: home.to_path_buf(),
            version: String::new(),
            source: String::new(),
            notes: Vec::new(),
        },
        data_root,
    )?;
    let overrides = environment
        .iter()
        .map(|(name, value)| (name.as_str(), std::ffi::OsStr::new(value)))
        .collect::<Vec<_>>();
    let mut versions = Vec::new();
    for command in ["java.exe", "javac.exe"] {
        let program = home.join("bin").join(command);
        let prefix = if command == "javac.exe" { "-J" } else { "" };
        let temporary = format!("{prefix}-Djava.io.tmpdir={}", environment["TEMP"]);
        let private_home = format!("{prefix}-Duser.home={}", environment["HOME"]);
        let output = run_capture_sanitized(
            &program,
            &[&temporary, &private_home, "-version"],
            Duration::from_secs(6),
            &overrides,
            &["JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS"],
        )?;
        if !output.status.success() {
            return Err(AppError::Message(format!(
                "{}：{}",
                program.display(),
                output_text(&output)
            )));
        }
        versions.push(
            java_version_from_output(&output_text(&output)).ok_or_else(|| {
                AppError::Message(format!("{} 没有返回可识别的 Java 版本", program.display()))
            })?,
        );
    }
    Ok((versions[0].0.clone(), versions[0].1, versions[1].1))
}

fn java_version_from_output(text: &str) -> Option<(String, u32)> {
    let pattern = regex::Regex::new(
        r#"(?m)(?:openjdk|java|javac)(?:\s+version)?\s+"?([0-9][0-9A-Za-z._+\-]*)"#,
    )
    .ok()?;
    let version = pattern.captures(text)?.get(1)?.as_str().to_string();
    let mut parts = version.split(['.', '_', '-', '+']);
    let first = parts.next()?.parse::<u32>().ok()?;
    let major = if first == 1 {
        parts.next()?.parse().ok()?
    } else {
        first
    };
    Some((version, major))
}

pub(crate) fn maven_child_environment(
    current: &EnvironmentMap,
    runtime: &MavenJavaRuntime,
) -> EnvironmentMap {
    let mut environment = current.clone();
    let inherited_path = get_case_insensitive(current, "PATH")
        .cloned()
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    let isolated = [
        "MAVEN_BATCH_PAUSE",
        "MAVEN_TERMINATE_CMD",
        "MAVEN_OPTS",
        "MAVEN_DEBUG_OPTS",
        "MAVEN_ARGS",
        "MAVEN_BASEDIR",
        "MAVEN_PROJECTBASEDIR",
        "MAVEN_HOME",
        "M2_HOME",
        "JVM_CONFIG_MAVEN_PROPS",
        "JAVA_TOOL_OPTIONS",
        "_JAVA_OPTIONS",
        "JDK_JAVA_OPTIONS",
    ];
    for name in ["JAVA_HOME", "PATH", "MAVEN_SKIP_RC"]
        .into_iter()
        .chain(isolated)
    {
        environment.retain(|existing, _| !existing.eq_ignore_ascii_case(name));
    }
    environment.insert(
        "JAVA_HOME".into(),
        runtime.home.to_string_lossy().into_owned(),
    );
    environment.insert(
        "PATH".into(),
        format!("{};{inherited_path}", runtime.home.join("bin").display()),
    );
    environment.insert("MAVEN_SKIP_RC".into(), "1".into());
    // Version checks must not load user startup hooks or inherited Java agents.
    for name in isolated {
        environment.insert(name.into(), String::new());
    }
    environment
}

pub(crate) fn maven_verification_environment(
    current: &EnvironmentMap,
    runtime: &MavenJavaRuntime,
    data_root: &Path,
) -> AppResult<EnvironmentMap> {
    let mut environment = maven_child_environment(current, runtime);
    let root = data_root.join("cache/maven-verification");
    let temp = root.join("temp");
    let home = root.join("home");
    fs::create_dir_all(&temp)?;
    fs::create_dir_all(&home)?;
    for (name, path) in [
        ("TEMP", &temp),
        ("TMP", &temp),
        ("TMPDIR", &temp),
        ("HOME", &home),
        ("USERPROFILE", &home),
        ("MAVEN_USER_HOME", &home),
    ] {
        environment.retain(|existing, _| !existing.eq_ignore_ascii_case(name));
        environment.insert(name.into(), path.to_string_lossy().into_owned());
    }
    environment.insert(
        "MAVEN_OPTS".into(),
        format!(
            "-Djava.io.tmpdir=\"{}\" -Duser.home=\"{}\"",
            temp.display(),
            home.display()
        ),
    );
    Ok(environment)
}

pub(crate) fn maven_version_hint(root: &Path) -> Option<String> {
    fs::read_dir(root.join("lib"))
        .ok()?
        .take(512)
        .filter_map(Result::ok)
        .find_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_prefix("maven-core-")?
                .strip_suffix(".jar")
                .map(String::from)
        })
}

/// The Windows Platform-Tools package keeps these libraries beside adb.exe.
/// Check them without starting a process, including when a version was cached.
pub fn validate_runtime_dependencies(tool_id: &str, executable: &Path) -> AppResult<()> {
    if tool_id != "adb" {
        return Ok(());
    }
    let directory = executable
        .parent()
        .ok_or_else(|| AppError::UnsafePath(executable.to_path_buf()))?;
    let missing = ["AdbWinApi.dll", "AdbWinUsbApi.dll"]
        .into_iter()
        .filter(|name| !directory.join(name).is_file())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(AppError::Message(format!(
        "ADB 安装目录不完整（{}）：缺少 {}。请完整解压官方 Platform-Tools 包，将 adb.exe 和配套 DLL 保留在同一目录。",
        directory.display(),
        missing.join("、")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn jdk_fixture(root: &Path, name: &str) -> PathBuf {
        let home = root.join(name);
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin/java.exe"), b"java fixture").unwrap();
        fs::write(home.join("bin/javac.exe"), b"javac fixture").unwrap();
        home
    }

    #[test]
    fn maven_uses_a_managed_jdk_when_java_home_is_stale_without_mutating_environment() {
        let root = tempfile::tempdir().unwrap();
        let home = jdk_fixture(root.path(), "jdk-21");
        let current = EnvironmentMap::from([
            (
                "JAVA_HOME".into(),
                root.path().join("removed-jdk").display().to_string(),
            ),
            ("PATH".into(), String::new()),
        ]);
        let original = current.clone();
        let runtime = resolve_maven_java_candidates(
            "3.9.16",
            &current,
            std::slice::from_ref(&home),
            |candidate| {
                assert_eq!(candidate, home);
                Ok(("21.0.2".into(), 21, 21))
            },
        )
        .unwrap();
        assert_eq!(runtime.home, home);
        assert!(runtime.notes.iter().any(|note| note.contains("缺少")));
        assert_eq!(current, original);
        let child = maven_child_environment(&current, &runtime);
        assert_eq!(
            get_case_insensitive(&child, "JAVA_HOME").unwrap(),
            &home.display().to_string()
        );
        assert_eq!(current, original);
        assert!(runtime.plan_warning().contains("只作用于 Maven 验证子进程"));
    }

    #[test]
    fn maven_respects_valid_java_home_then_path_and_requires_suitable_java_and_javac() {
        let root = tempfile::tempdir().unwrap();
        let old = jdk_fixture(root.path(), "jdk-8");
        let modern = jdk_fixture(root.path(), "jdk-17");
        let current = EnvironmentMap::from([
            ("JAVA_HOME".into(), old.display().to_string()),
            ("Path".into(), modern.join("bin").display().to_string()),
        ]);
        let mut visited = Vec::new();
        let runtime = resolve_maven_java_candidates("3.9.16", &current, &[], |home| {
            visited.push(home.to_path_buf());
            Ok(("1.8.0_442".into(), 8, 8))
        })
        .unwrap();
        assert_eq!(runtime.home, old);
        assert_eq!(visited.as_slice(), std::slice::from_ref(&old));
        let runtime = resolve_maven_java_candidates("4.0.0-rc-5", &current, &[], |home| {
            if home == old {
                Ok(("1.8.0_442".into(), 8, 8))
            } else {
                Ok(("17.0.15".into(), 17, 17))
            }
        })
        .unwrap();
        assert_eq!(
            runtime.home,
            crate::paths::canonicalize_simplified(&modern).unwrap()
        );
        assert!(runtime.source.contains("PATH"));
        assert!(
            resolve_maven_java_candidates("3.9.16", &current, &[], |_| Ok(("21".into(), 21, 8)))
                .is_err()
        );
        let error = resolve_maven_java_candidates("3.9.16", &current, &[], |_| {
            Err(AppError::Message("blocked executable".into()))
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("blocked executable"));
    }

    #[test]
    fn maven_prioritizes_fresh_registry_jdk_but_accepts_valid_process_only_fallbacks() {
        let root = tempfile::tempdir().unwrap();
        let fresh = jdk_fixture(root.path(), "fresh-21");
        let session = jdk_fixture(root.path(), "session-17");
        let current = EnvironmentMap::from([
            ("JAVA_HOME".into(), fresh.display().to_string()),
            ("PATH".into(), String::new()),
        ]);
        let inherited = EnvironmentMap::from([
            (
                "JAVA_HOME".into(),
                root.path().join("gone").display().to_string(),
            ),
            ("PATH".into(), session.join("bin").display().to_string()),
        ]);
        let runtime = resolve_maven_java_candidates_with_process(
            "3.9.16",
            &current,
            Some(&inherited),
            &[],
            |home| {
                assert_eq!(home, fresh);
                Ok(("21".into(), 21, 21))
            },
        )
        .unwrap();
        assert_eq!(runtime.home, fresh);
        let current = EnvironmentMap::from([("PATH".into(), String::new())]);
        let runtime = resolve_maven_java_candidates_with_process(
            "3.9.16",
            &current,
            Some(&inherited),
            &[],
            |home| {
                assert_eq!(
                    home,
                    crate::paths::canonicalize_simplified(&session).unwrap()
                );
                Ok(("17".into(), 17, 17))
            },
        )
        .unwrap();
        assert!(runtime.source.contains("当前进程 PATH"));
    }

    #[test]
    fn maven_diagnoses_quoted_bin_home_and_ignores_jres_and_arbitrary_path_shims() {
        let root = tempfile::tempdir().unwrap();
        let home = jdk_fixture(root.path(), "jdk-17");
        let current = EnvironmentMap::from([
            (
                "java_home".into(),
                format!("\"{}\"", home.join("bin").display()),
            ),
            ("PATH".into(), String::new()),
        ]);
        let runtime =
            resolve_maven_java_candidates("3.9.16", &current, &[], |_| Ok(("17".into(), 17, 17)))
                .unwrap();
        assert_eq!(runtime.home, home);
        assert!(runtime.notes.iter().any(|note| note.contains("引号")));
        assert!(runtime.notes.iter().any(|note| note.contains("指向 bin")));
        fs::remove_file(home.join("bin/javac.exe")).unwrap();
        fs::write(home.join("release"), b"JAVA_VERSION=\"21\"").unwrap();
        let shim = root.path().join("shims");
        fs::create_dir(&shim).unwrap();
        fs::write(shim.join("java.exe"), b"arbitrary shim").unwrap();
        let current = EnvironmentMap::from([
            ("JAVA_HOME".into(), home.display().to_string()),
            ("PATH".into(), shim.display().to_string()),
        ]);
        let error = resolve_maven_java_candidates("3.9.16", &current, &[], |_| {
            panic!("JRE or arbitrary shim must not execute")
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("完整 JDK 8+"));
        assert!(error.contains("javac.exe"));
    }

    #[test]
    fn java_output_supports_legacy_and_current_versions_and_maven_baselines() {
        for (output, expected) in [
            ("java version \"1.8.0_442\"", 8),
            ("openjdk version \"17.0.15\" 2025-04-15", 17),
            ("openjdk 27-ea 2026-09-15", 27),
            ("javac 21.0.6", 21),
        ] {
            assert_eq!(java_version_from_output(output).unwrap().1, expected);
        }
        assert!(java_version_from_output("JAVA_HOME is not defined correctly").is_none());
        assert_eq!(maven_minimum_java("3.9.16"), 8);
        assert_eq!(maven_minimum_java("4.0.0-alpha-12"), 8);
        assert_eq!(maven_minimum_java("4.0.0-alpha-13"), 17);
        assert_eq!(maven_minimum_java("4.0.0-rc-5"), 17);
    }

    #[test]
    fn maven_version_check_isolates_startup_hooks_and_java_options_in_the_child() {
        let current = EnvironmentMap::from([
            ("JAVA_HOME".into(), "old".into()),
            ("Path".into(), "original-path".into()),
            ("MAVEN_SKIP_RC".into(), "".into()),
            ("MAVEN_OPTS".into(), "-javaagent:foreign.jar".into()),
            ("JAVA_TOOL_OPTIONS".into(), "-javaagent:other.jar".into()),
            ("_JAVA_OPTIONS".into(), "-Duser.home=C:/foreign".into()),
            ("JDK_JAVA_OPTIONS".into(), "bad option".into()),
            ("MAVEN_BATCH_PAUSE".into(), "on".into()),
            ("MAVEN_HOME".into(), "E:/removed-maven".into()),
            ("M2_HOME".into(), "E:/removed-maven".into()),
        ]);
        let original = current.clone();
        let runtime = MavenJavaRuntime {
            home: PathBuf::from("E:/Environment/java/21"),
            version: "21".into(),
            source: String::new(),
            notes: Vec::new(),
        };
        let child = maven_child_environment(&current, &runtime);
        assert_eq!(child["MAVEN_SKIP_RC"], "1");
        for name in [
            "MAVEN_OPTS",
            "JAVA_TOOL_OPTIONS",
            "_JAVA_OPTIONS",
            "JDK_JAVA_OPTIONS",
            "MAVEN_BATCH_PAUSE",
            "MAVEN_HOME",
            "M2_HOME",
        ] {
            assert_eq!(child[name], "");
        }
        assert!(child["PATH"].ends_with(";original-path"));
        assert_eq!(
            child
                .keys()
                .filter(|name| name.eq_ignore_ascii_case("PATH"))
                .count(),
            1
        );
        assert_eq!(current, original);
    }

    #[test]
    fn maven_verification_uses_private_temp_home_and_controlled_java_properties() {
        let root = tempfile::tempdir().unwrap();
        let current = EnvironmentMap::from([
            ("TEMP".into(), "C:/foreign-temp".into()),
            ("USERPROFILE".into(), "C:/foreign-home".into()),
            ("MAVEN_OPTS".into(), "-javaagent:foreign.jar".into()),
        ]);
        let runtime = MavenJavaRuntime {
            home: root.path().join("jdk"),
            version: "21".into(),
            source: String::new(),
            notes: Vec::new(),
        };
        let environment = maven_verification_environment(&current, &runtime, root.path()).unwrap();
        assert!(Path::new(&environment["TEMP"]).starts_with(root.path()));
        assert!(Path::new(&environment["HOME"]).is_dir());
        assert_eq!(environment["HOME"], environment["USERPROFILE"]);
        assert!(environment["MAVEN_OPTS"].contains("-Djava.io.tmpdir="));
        assert!(environment["MAVEN_OPTS"].contains("-Duser.home="));
        assert!(!environment["MAVEN_OPTS"].contains("javaagent"));
        assert_eq!(current["TEMP"], "C:/foreign-temp");
    }

    #[test]
    fn adb_runtime_check_requires_both_companion_libraries() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("adb.exe");
        fs::write(&executable, b"test fixture").unwrap();
        let error = validate_runtime_dependencies("adb", &executable)
            .unwrap_err()
            .to_string();
        assert!(error.contains("AdbWinApi.dll"));
        assert!(error.contains("AdbWinUsbApi.dll"));
        assert!(error.contains(&temp.path().display().to_string()));

        fs::write(temp.path().join("AdbWinApi.dll"), b"api").unwrap();
        let error = validate_runtime_dependencies("adb", &executable)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("AdbWinApi.dll"));
        assert!(error.contains("AdbWinUsbApi.dll"));

        fs::write(temp.path().join("AdbWinUsbApi.dll"), b"usb").unwrap();
        assert!(validate_runtime_dependencies("adb", &executable).is_ok());
    }

    #[test]
    fn unrelated_tools_do_not_require_adb_libraries() {
        assert!(validate_runtime_dependencies("node", Path::new("node.exe")).is_ok());
    }
}
