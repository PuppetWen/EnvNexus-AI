# EnvNexus AI

EnvNexus AI 是面向 Windows 10/11 的开发环境与多版本工具链管理器，使用 Rust、Tauri 2 和 WebView2 构建。它把本机识别、官方版本查询、下载安装、默认版本切换、环境诊断、修复与应用更新放在统一且可回滚的流程中。

[下载最新版](https://github.com/PuppetWen/EnvNexus-AI/releases/latest) · [更新日志](CHANGELOG.md) · [使用说明](docs/USER_GUIDE.md) · [构建说明](docs/BUILDING.md)

## 主要能力

- 管理 Python、Java/JDK、Go、Rust、Node.js、Git、Maven、.NET SDK、Ruby、PHP、Android SDK/NDK、Gradle、CMake 和 ADB。
- 扫描电脑上的全部本地固定磁盘，不依赖固定用户名、桌面目录或预设安装路径。
- 运行工具自身的版本命令验证候选，按工具类型归类，并按数字版本倒序展示。
- 读取最新用户级和系统级 PATH，识别当前默认版本；安装或切换后立即刷新。
- 识别 pyenv-win、NVM for Windows、fnm、Volta、rustup、Jabba、goenv、rbenv 和 Uru 等版本管理器。
- 支持续传下载、哈希与签名校验、安全解压、事务安装、失败回滚和环境备份。
- 安装版可以选择目录并原地升级；便携版在原路径安全自替换。
- 支持托盘驻留、开机启动、多语言、五套主题及可选 AI 诊断增强。

## 全机扫描与增量刷新

首次点击“扫描整台电脑”或执行 `env-scan` 时，程序遍历所有本地固定磁盘，建立：

- `tool-executable-discovery.json`：工具可执行文件与受管安装清单索引；
- `tool-version-probes.json`：经过版本命令验证的文件指纹缓存；
- `last-environment-scan.json`：界面、托盘和命令行共用的扫描快照。

完整扫描不会因为性能优化而跳过版本验证。之后安装、切换、修复或执行 `env-refresh` 时：

1. 重新读取 Windows 用户级和系统级环境变量；
2. 检查可执行文件大小、修改时间和必要的伴随元数据；
3. 只重新运行新增或发生变化的工具；
4. 重新计算当前默认版本、诊断和排序。

如果工具由外部程序原地替换，文件指纹会失效并自动重新验证；如果需要完全复核，随时执行 `env-scan`。

## 性能策略

- App 启动不自动遍历磁盘，只读取上次快照。
- 空闲界面没有定时扫描或轮询。
- 扫描目录采用流式遍历，只保存匹配的候选路径。
- 相同配置根目录只遍历一次，Android 共享根目录不会为多个组件重复扫描。
- 增量刷新避免反复启动大量工具进程。
- 窗口失焦或进入托盘时，WebView2 使用低内存目标；恢复窗口后切回正常目标。后台下载、更新和 Rust 后端任务不会暂停。

在开发机现有全机索引上，首次增量探测约 33.2 秒，建立缓存后的同结果刷新约 2.4 秒；两次均识别 15 类工具和 161 个安装版本。相同的最小化/失焦测试中，整个 7 进程树的平均工作集由 0.1.3 的 436.1 MiB 降至 0.1.4 的 101.6 MiB。私有提交量基本不变；这里的下降指物理驻留工作集。为保证默认版本准确，当前 PATH 中实际生效的工具仍会每次重新验证。不同电脑的磁盘、工具数量和版本管理器会影响实际结果。

## 下载

从 [GitHub Releases](https://github.com/PuppetWen/EnvNexus-AI/releases/latest) 获取：

- `EnvNexus-AI_0.1.4_x64-setup.exe`：安装版，可选择安装目录；已有安装会复用原目录覆盖升级。
- `EnvNexus-AI_0.1.4_x64-portable.exe`：便携版，可放在任意可写目录运行。
- `SHA256SUMS.txt`：发布文件 SHA-256 校验值。
- `latest.json` 与 `.sig`：应用内自动更新使用的签名元数据。

## 常用命令

在 App 的“命令说明”页面生成并启用 CMD 脚本后，新开 CMD 或 PowerShell：

```powershell
env-tools
env-scan
env-refresh
python-list
jdk-list
jdk-root set "E:\DevelopmentTools\Java"
jdk-use "E:\DevelopmentTools\Java\java\21.0.8"
env-diagnose
```

- `env-scan`：重建全机索引并强制验证所有候选。
- `env-refresh`：使用索引和文件指纹快速刷新。
- `*-list`：只读取最近快照。
- `install`、`use`、`repair` 和 `uninstall` 默认只预览；追加 `--yes` 后才执行。

## 安全边界

- 不会自动迁移或整理现有工具目录。
- 不会静默修改系统级 PATH。
- 写操作默认限制在当前用户范围。
- 环境变更前提供差异预览和备份，失败时执行回滚。
- AI 只能分析用户明确选择的诊断信息，不能绕过本地计划、校验和确认流程。
- API 密钥在 Windows 下使用 DPAPI 按当前用户加密。

## 从源码构建

需要 Windows 10/11、Node.js、pnpm、Rust stable、Visual Studio 2022 C++ Build Tools 和 Windows SDK：

```powershell
pnpm install
.\scripts\Verify.ps1
.\scripts\Invoke-Rust.ps1 -Task tauri
.\scripts\Prepare-Release.ps1
```

更新产物必须使用 Tauri updater 私钥签名。私钥只能保存在本机或安全的 CI Secret 中，不能提交到 Git 仓库。

## 相关文档

- [更新日志](CHANGELOG.md)
- [完整使用说明](docs/USER_GUIDE.md)
- [架构设计](docs/ARCHITECTURE.md)
- [安全模型](docs/SECURITY_MODEL.md)
- [构建与发布](docs/BUILDING.md)
- [测试报告](docs/TEST_REPORT.md)
- [第三方许可](THIRD_PARTY_NOTICES.md)
