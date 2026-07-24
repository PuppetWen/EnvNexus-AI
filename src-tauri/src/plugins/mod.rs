use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;

use crate::{
    error::{AppError, AppResult},
    model::VersionCatalog,
    sources,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionSourceKind {
    Python,
    Adoptium,
    Go,
    Rust,
    Node,
    GitForWindows,
    AndroidSdk,
    AndroidNdk,
    Gradle,
    CMake,
    Adb,
    Maven,
    DotNet,
    Ruby,
    Php,
}

#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub category: &'static str,
    pub icon: &'static str,
    pub executable: &'static str,
    pub version_args: &'static [&'static str],
    pub version_pattern: &'static str,
    pub path_depth: usize,
    pub home_variables: &'static [&'static str],
    pub source: VersionSourceKind,
}

#[async_trait]
pub trait ToolPlugin: Send + Sync {
    fn descriptor(&self) -> &ToolDescriptor;

    async fn fetch_available_versions(&self, client: &reqwest::Client)
    -> AppResult<VersionCatalog>;

    fn supports_install(&self) -> bool {
        true
    }

    fn supports_switch(&self) -> bool {
        true
    }

    fn supports_repair(&self) -> bool {
        true
    }

    fn supports_uninstall(&self) -> bool {
        true
    }
}

pub struct BuiltinPlugin {
    descriptor: ToolDescriptor,
}

#[async_trait]
impl ToolPlugin for BuiltinPlugin {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn fetch_available_versions(
        &self,
        client: &reqwest::Client,
    ) -> AppResult<VersionCatalog> {
        sources::fetch(client, self.descriptor.id, self.descriptor.source).await
    }
}

#[derive(Clone)]
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn ToolPlugin>>,
    by_id: HashMap<&'static str, Arc<dyn ToolPlugin>>,
}

impl PluginRegistry {
    pub fn builtin() -> Self {
        let descriptors = builtin_descriptors();
        let plugins = descriptors
            .into_iter()
            .map(|descriptor| Arc::new(BuiltinPlugin { descriptor }) as Arc<dyn ToolPlugin>)
            .collect::<Vec<_>>();
        let by_id = plugins
            .iter()
            .map(|plugin| (plugin.descriptor().id, Arc::clone(plugin)))
            .collect();
        Self { plugins, by_id }
    }

    pub fn all(&self) -> &[Arc<dyn ToolPlugin>] {
        &self.plugins
    }

    pub fn get(&self, id: &str) -> AppResult<Arc<dyn ToolPlugin>> {
        self.by_id
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::UnknownTool(id.to_string()))
    }
}

fn builtin_descriptors() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            id: "python",
            display_name: "Python",
            category: "运行时",
            icon: "Py",
            executable: "python.exe",
            version_args: &["--version"],
            version_pattern: r"(?i)Python\s+([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)",
            path_depth: 0,
            home_variables: &["PYENV_ROOT", "PYENV"],
            source: VersionSourceKind::Python,
        },
        ToolDescriptor {
            id: "java",
            display_name: "Java / JDK",
            category: "运行时",
            icon: "Jv",
            executable: "java.exe",
            version_args: &["-version"],
            version_pattern: r#"(?i)(?:java|openjdk) version "([^"]+)""#,
            path_depth: 1,
            home_variables: &["JAVA_HOME"],
            source: VersionSourceKind::Adoptium,
        },
        ToolDescriptor {
            id: "go",
            display_name: "Go",
            category: "编译工具链",
            icon: "Go",
            executable: "go.exe",
            version_args: &["version"],
            version_pattern: r"\bgo([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)\b",
            path_depth: 1,
            home_variables: &["GOROOT"],
            source: VersionSourceKind::Go,
        },
        ToolDescriptor {
            id: "rust",
            display_name: "Rust",
            category: "编译工具链",
            icon: "Rs",
            executable: "rustc.exe",
            version_args: &["--version"],
            version_pattern: r"\brustc\s+([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)",
            path_depth: 1,
            home_variables: &["CARGO_HOME", "RUSTUP_HOME"],
            source: VersionSourceKind::Rust,
        },
        ToolDescriptor {
            id: "node",
            display_name: "Node.js",
            category: "运行时",
            icon: "Js",
            executable: "node.exe",
            version_args: &["--version"],
            version_pattern: r"\bv?([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)",
            path_depth: 0,
            home_variables: &["NVM_HOME", "NVM_SYMLINK"],
            source: VersionSourceKind::Node,
        },
        ToolDescriptor {
            id: "git",
            display_name: "Git",
            category: "版本控制",
            icon: "Git",
            executable: "git.exe",
            version_args: &["--version"],
            version_pattern: r"(?i)git version\s+([0-9]+(?:\.[0-9]+){1,4}(?:\.windows\.[0-9]+)?)",
            path_depth: 1,
            home_variables: &[],
            source: VersionSourceKind::GitForWindows,
        },
        ToolDescriptor {
            id: "android-sdk",
            display_name: "Android SDK",
            category: "Android",
            icon: "Sdk",
            executable: "sdkmanager.bat",
            version_args: &["--version"],
            version_pattern: r"(?m)^\s*([0-9]+(?:\.[0-9]+){0,3})\s*$",
            path_depth: 3,
            home_variables: &["ANDROID_HOME", "ANDROID_SDK_ROOT"],
            source: VersionSourceKind::AndroidSdk,
        },
        ToolDescriptor {
            id: "android-ndk",
            display_name: "Android NDK",
            category: "Android",
            icon: "Ndk",
            executable: "ndk-build.cmd",
            version_args: &["--version"],
            version_pattern: r"(?i)(?:GNU Make|ndk-build)\s+([0-9]+(?:\.[0-9]+){1,3})",
            path_depth: 0,
            home_variables: &["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"],
            source: VersionSourceKind::AndroidNdk,
        },
        ToolDescriptor {
            id: "gradle",
            display_name: "Gradle",
            category: "构建工具",
            icon: "Gr",
            executable: "gradle.bat",
            version_args: &["--version"],
            version_pattern: r"(?m)^Gradle\s+([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)",
            path_depth: 1,
            home_variables: &["GRADLE_HOME"],
            source: VersionSourceKind::Gradle,
        },
        ToolDescriptor {
            id: "cmake",
            display_name: "CMake",
            category: "构建工具",
            icon: "Cm",
            executable: "cmake.exe",
            version_args: &["--version"],
            version_pattern: r"(?i)cmake version\s+([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)",
            path_depth: 1,
            home_variables: &["CMAKE_HOME"],
            source: VersionSourceKind::CMake,
        },
        ToolDescriptor {
            id: "adb",
            display_name: "ADB",
            category: "Android",
            icon: "Db",
            executable: "adb.exe",
            version_args: &["version"],
            version_pattern: r"(?i)Android Debug Bridge version\s+([0-9]+(?:\.[0-9]+){1,3})",
            path_depth: 1,
            home_variables: &["ANDROID_HOME", "ANDROID_SDK_ROOT"],
            source: VersionSourceKind::Adb,
        },
        ToolDescriptor {
            id: "maven",
            display_name: "Apache Maven",
            category: "构建工具",
            icon: "Mv",
            executable: "mvn.cmd",
            version_args: &["--version"],
            version_pattern: r"(?i)Apache Maven\s+([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)",
            path_depth: 1,
            home_variables: &["MAVEN_HOME", "M2_HOME"],
            source: VersionSourceKind::Maven,
        },
        ToolDescriptor {
            id: "dotnet",
            display_name: ".NET SDK",
            category: "运行时",
            icon: ".N",
            executable: "dotnet.exe",
            version_args: &["--version"],
            version_pattern: r"(?m)^\s*([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)\s*$",
            path_depth: 0,
            home_variables: &["DOTNET_ROOT", "DOTNET_ROOT_X64"],
            source: VersionSourceKind::DotNet,
        },
        ToolDescriptor {
            id: "ruby",
            display_name: "Ruby",
            category: "运行时",
            icon: "Rb",
            executable: "ruby.exe",
            version_args: &["--version"],
            version_pattern: r"(?i)\bruby\s+([0-9]+(?:\.[0-9]+){1,3}(?:p[0-9]+)?[A-Za-z0-9.+-]*)",
            path_depth: 1,
            home_variables: &["RUBY_HOME"],
            source: VersionSourceKind::Ruby,
        },
        ToolDescriptor {
            id: "php",
            display_name: "PHP",
            category: "运行时",
            icon: "Php",
            executable: "php.exe",
            version_args: &["--version"],
            version_pattern: r"(?i)\bPHP\s+([0-9]+(?:\.[0-9]+){1,3}[A-Za-z0-9.+-]*)",
            path_depth: 0,
            home_variables: &["PHP_HOME"],
            source: VersionSourceKind::Php,
        },
    ]
}
