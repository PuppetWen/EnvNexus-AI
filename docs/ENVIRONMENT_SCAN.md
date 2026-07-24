# EnvNexus AI 开发环境扫描报告

扫描时间：2026-07-23  
扫描方式：PowerShell 只读命令、`vswhere`、工具自身 `--version`/`-version` 输出  
扫描范围：当前项目目录、构建前置条件、`E:\Environment` 顶层目录、与开发工具有关的用户级/系统级环境变量  

## 结论

- 当前项目目录为空，不是 Git 仓库。
- Windows 11 x64，版本 `10.0.26200`。
- `E:` 可用空间约 430.68 GiB，适合存放项目隔离的开发依赖和构建产物。
- Tauri 的原生构建条件部分具备：
  - Visual Studio 2022 Community `17.14.24` 和 Enterprise `17.13.6` 已安装；
  - 两套实例均包含 MSVC x64/x86 C++ 工具；
  - Windows SDK 注册值指向不存在的 `E:\Windows Kits\10`，标准链接最初因缺少 `kernel32.lib` 失败；
  - Visual Studio NuGet SDK Build Tools 中仍存在可用的 `RC.EXE`；
  - Edge WebView2 Runtime `150.0.4078.83` 已安装。
- 已有 Rust `1.75.0 (MSVC)`，但对当前 Tauri 2 依赖偏旧；项目已在 `.devtools` 使用隔离的 Rust `1.97.1`。
- `cargo-tauri`、CMake 和 Ninja 不在当前 PATH。桌面 MVP 不需要 CMake/Ninja；Tauri CLI 将作为项目依赖安装。
- Node.js `25.2.1`、npm `11.12.1` 可用。项目使用本地 `node_modules` 和项目内 pnpm store。

## 已验证的工具

| 工具 | 实际版本 | 当前解析位置 |
| --- | --- | --- |
| Rust | 1.75.0 | `C:\Users\puppet\.cargo\bin` |
| Node.js | 25.2.1 | `E:\Environment\nvm\nodejs` |
| Go | 1.25.4 | `E:\Environment\Go\bin` |
| Java | 1.8.0_281 | `E:\Environment\Java1.8\jdk1.8.0_281` |
| Python | 3.11.1 | `E:\Environment\pyenv-win\pyenv-win\shims` |
| Git for Windows | 2.53.0.windows.2 | `D:\CommonTools\Git` |

## 已发现的多版本与候选根目录

- Node.js：`E:\Environment\nvm` 下发现 `18.15.0`、`18.16.0`、`18.18.0`、`25.2.1`。
- Java：发现 `E:\Environment\Java\jdk-17` 与 `E:\Environment\Java1.8\jdk1.8.0_281`。
- Python：存在 pyenv-win，同时用户 PATH 还包含多套位于 `E:` 和 `C:` 的 Python 3.6/3.10/3.11。
- Android：有效 SDK 路径位于 `E:\DevelopmentTools\Android\SDK`；`E:\Environment\Android` 不存在。
- Git：实际位置是 `D:\CommonTools\Git`。

## 已发现但未修改的环境问题

这些结果将作为 EnvNexus AI 诊断规则的真实测试样例；扫描没有执行修复。

1. 用户 PATH 同时包含多套 Python 3.6、3.10、3.11，命令解析结果受顺序影响。
2. `NVM_HOME` 和 `NVM_SYMLINK` 同时出现在用户级与系统级环境变量中。
3. 系统 PATH 包含疑似被分号截断的 `D:\Com;onTools\Git\cmd`。
4. 系统 PATH 中存在重复的 Git、Windows 系统目录、NVM 和 npm 条目，以及空条目。
5. Java 17 已存在，但系统 `JAVA_HOME` 和 PATH 仍固定指向 Java 8。
6. 用户 `GOPATH` 位于 `C:\Users\puppet\go`；这不是错误，但与“尽量避免写入 C 盘”的目标不一致，后续只能在用户确认后迁移。
7. Android SDK 分散在 `E:\DevelopmentTools\Android`，尚未统一到用户指定的 Android 根目录。

## 开发期间的写入约束

- 不整理或迁移现有 `E:\Environment`。
- 缺少的 Rust 工具链、包管理器缓存和构建缓存放在项目的 `.devtools` 下。
- JS 依赖放在项目的 `node_modules` 下。
- 开发态 App 数据默认放在项目的 `.envnexus-ai-data` 下；如果只存在旧版 `.envpilot-data`，升级版会兼容读取。
- 不主动修改 HKLM、系统 PATH、Windows 功能或现有工具目录。
- 缺失的 Windows SDK 头文件/库使用项目内 `.devtools/xwin-sdk` 补齐；系统 SDK 注册表未修改。
- 若打包 MSI 时发现 VBSCRIPT 功能缺失，先报告并征求确认；优先构建不依赖该功能的 NSIS 安装包。

## 可复现性说明

本报告只记录实际命令输出中已经验证的事实。官方在线版本属于易变信息，不固化在本报告中，由 App 的版本源插件实时查询并标明查询时间。
