# EnvNexus AI

EnvNexus AI 是面向 Windows 10/11 的开发环境与多版本工具链管理器，使用 Rust、Tauri 2 和 WebView2 构建。它把本机识别、官方版本查询、下载安装、默认版本切换、环境诊断、修复与应用更新放在统一且可回滚的流程中。

[下载最新版](https://github.com/PuppetWen/EnvNexus-AI/releases/latest) · [更新日志](CHANGELOG.md) · [使用说明](docs/USER_GUIDE.md) · [构建说明](docs/BUILDING.md)

当前版本 **[0.1.9](https://github.com/PuppetWen/EnvNexus-AI/releases/tag/v0.1.9)** 已公开发布：统一修复 15 类工具安装确认、进度、后台任务、通知和错误中的路径/多行文字显示，并改进工具输出解码与 CMD 脚本编码处理。Windows CI3 的 205 项 Rust、77 项前端、4 项 Node 测试和真实安装回归已通过，独立签名验证、六项公开资产及 latest 更新入口验收也已通过；320 个浏览器 mock 场景通过，未运行真实安装。合并提交与版本标签一致，完整证据和边界见[测试报告](docs/TEST_REPORT.md)。

## 主要能力

- 管理 Python、Java/JDK、Go、Rust、Node.js、Git、Maven、.NET SDK、Ruby、PHP、Android SDK/NDK、Gradle、CMake 和 ADB。
- Python 查询合并官网发布数据库、历史归档及官方 CPython 版本标签，展示 0.9、1.x、Python 2、旧 Python 3 的稳定版和预览版；仅源码、缺包和不能自动安装的条目也会保留并说明原因。
- 官方版本支持关键词、发布类型和“仅可安装”筛选，可按版本或发布日期从新到旧、从旧到新排序。
- Python 优先采用官方完整 PythonCore ZIP；旧版受支持的 Wise EXE / MSI / WiX Burn EXE 静态提取到自选目录，保留完整标准库和随包能力。
- 按需手动扫描全部本地固定磁盘，不依赖固定用户名、桌面目录或预设安装路径。
- 官方版本清单优先显示已有缓存，过期后在后台刷新；合并重复查询，安装前补齐选中版本的校验信息。
- 运行工具自身的版本命令验证候选，按工具类型归类，并按数字版本倒序展示。
- 读取最新用户级和系统级 PATH，识别当前默认版本；安装或切换后立即刷新。
- 安装完成后展示 PATH 和工具变量配置计划，可确认设为默认版本或仅保留安装。
- 识别 pyenv-win、NVM for Windows、fnm、Volta、rustup、Jabba、goenv、rbenv 和 Uru 等版本管理器。
- 支持续传下载、哈希与签名校验、安全解压、事务安装、失败回滚和环境备份。
- 安装版可以选择目录并原地升级；便携版在原路径安全自替换。
- 支持托盘驻留、开机启动、多语言、五套主题及可选 AI 诊断增强。

Maven 需要满足该版本要求的完整 JDK，只有 JRE 或失效的 `JAVA_HOME` 不够。安装和修复会先运行 `java.exe` 与 `javac.exe` 检查依赖，说明临时使用的 JDK；缺少可用 JDK 时请先在 Java 页面安装或配置。临时验证不会自动修改全局 Java，终端默认版本仍需单独确认配置。

## Python 历史版本

官方目录默认保留全部获取到的记录，稳定版、预览版、历史归档和是否可安装分别筛选。没有发布日期的归档仍可查询，按日期排序时排在已知日期之后。版本目录来自官网批量 API、归档总表和官方 CPython 版本标签，不会逐个版本发送数百次请求，也不会因某个版本不能安装而将其隐藏。CPython 标签通过 Git v2 `ls-refs` 批量查询；0.1.7 的历史网络样本返回 657 个版本标签，标签响应约 42 KB，数量和体积会随上游变化，不代表固定性能承诺。

完整 PythonCore ZIP 及其官方 SHA-256 优先；历史 Python 2.0–2.3 的 Wise EXE、Python 2.4 起的 MSI、Python 3.5 起的 WiX Burn EXE 在有效官方校验元数据齐全时可静态解包，不启动厂商安装器、MSI 自定义动作或全局安装流程。0.1.7 CI7 已通过十个历史版本的隔离安装验收。仅源码、缺少 Windows 包或缺少可验证下载元数据的版本显示对应的不可自动安装原因；源码标签本身不能证明该版没有 Windows 包。官网只提供 MD5 的历史包会在安装计划中明确说明弱校验的边界，不冒充 SHA-256 或发行签名验证。

Python 3.6 之前的 Windows 运行时及随包 Tcl/Tk 需要仅含 ASCII 字符的安装目录，例如 `E:\Environment`。安装或修复计划会提前拒绝含中文、重音字符等不兼容路径，执行阶段在启动旧解释器前再次复核。0.1.7 CI7 已通过这些路径拒绝检查，并以 Python `3.10.11` 通过含中文和重音字符目录的安装与 pip/venv 验收。

安装保留对应发行包的完整标准库和随包能力：正式发行包中的 Python 2.7.9+、Python 3.4+ 使用随包 pip/ensurepip，Python 3.3+ 保留内置 venv；预览版按实际组件验收，部分早期 alpha 尚无 venv 或 ensurepip。原包没有的功能不会被伪造，不会联网补装最新版 pip；诊断也会区分原本不具备的功能和真正损坏的安装。旧版解释器及第三方包能否运行仍取决于当前 Windows 与其兼容性。

历史 pip 从原包随带的 wheel 离线初始化，使用受管根目录内的短私有临时目录并隔离外部配置；安装后验证最终路径的 pip 命令和该版本具备的 venv 能力。[0.1.7 CI7 功能验证](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/39025514788)已通过 184 项 Rust、62 项前端、4 项 Node 测试、十个历史 Python 安装、现代 Python 新装与旧嵌入式安装原路径修复，以及 15/15 类工具真实安装；[独立草稿签名验证](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/39027005455)也已通过。[v0.1.7](https://github.com/PuppetWen/EnvNexus-AI/releases/tag/v0.1.7) 已公开发布，六项公开资产及自动更新入口回读均通过，范围和未验证边界见[测试报告](docs/TEST_REPORT.md)。

## 全机扫描与增量刷新

App 启动不遍历全盘。首次保存安装目录时，即使没有全机索引，也只检查配置目录和当前 PATH 中的工具，不会自动启动全盘扫描。

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

[v0.1.9](https://github.com/PuppetWen/EnvNexus-AI/releases/tag/v0.1.9) 已公开发布，CI、独立签名及公开下载验收通过：

- [EnvNexus-AI_0.1.9_x64-setup.exe](https://github.com/PuppetWen/EnvNexus-AI/releases/download/v0.1.9/EnvNexus-AI_0.1.9_x64-setup.exe)：安装版，可选择安装目录；已有安装会复用原目录覆盖升级。
- [EnvNexus-AI_0.1.9_x64-portable.exe](https://github.com/PuppetWen/EnvNexus-AI/releases/download/v0.1.9/EnvNexus-AI_0.1.9_x64-portable.exe)：便携版，可放在任意可写目录运行。
- `SHA256SUMS.txt`：发布文件 SHA-256 校验值。
- `latest.json` 与 `.sig`：应用内自动更新使用的签名元数据。

0.1.5 的受管 Python 安装使用嵌入式包，不包含完整 pip/venv 开发环境。升级后，旧的受限版本清单缓存自动失效；对仍有对应受支持官方完整发行包的版本，可在版本卡片点击“修复”，在原安装路径迁移到完整运行时。修复结束后单独确认 PATH/默认版本配置，或选择仅保留安装。没有可用完整包或有效官方校验信息时不会退回嵌入式包；诊断会标出对应版本中真正缺失的受管组件。

## 常用命令

在 App 的“命令说明”页面生成并启用 CMD 脚本后，新开 CMD 或 PowerShell。升级至 0.1.9 后需重新生成旧脚本，以应用 UTF-8 调用包装：

```powershell
env-tools
env-scan
env-refresh
python-list
python-versions --query 2.7 --channel stable --installable --sort version-asc
python-versions --channel preview --sort date-desc
jdk-list
jdk-root set "E:\DevelopmentTools\Java"
jdk-use "E:\DevelopmentTools\Java\java\21.0.8"
env-diagnose
```

- `env-scan`：重建全机索引并强制验证所有候选。
- `env-refresh`：使用索引和文件指纹快速刷新。
- `*-list`：只读取最近快照。
- `*-versions`：查询官方目录；`--query` 搜索版本号，`--channel` 选择 `all/stable/preview/archive`，`--installable` 仅显示可安装项，`--sort` 接受 `version-desc/version-asc/date-desc/date-asc`。
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
.\scripts\Use-ProjectCache.ps1
pnpm install --frozen-lockfile
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
