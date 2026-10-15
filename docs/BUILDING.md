# 构建说明

## 支持平台

- Windows 10/11 x64；
- Rust MSVC target；
- Tauri 2 / WebView2。

## 标准前置条件

1. Visual Studio 2022，安装“使用 C++ 的桌面开发”工作负载；
2. Windows 10 或 Windows 11 SDK；
3. Rust stable，至少满足 `rust-version = 1.88`；
4. Node.js 24（CI 使用的版本）；
5. pnpm 11。

`scripts/Invoke-Rust.ps1` 优先使用 `.devtools/rustup` 中的项目工具链；没有项目工具链时使用当前终端的 Cargo。它支持标准 Visual Studio 开发环境，也可使用项目内 xwin SDK、Rust 自带的 `rust-lld.exe`，并通过 `-LlvmBin` 和 `-ResourceCompiler` 指定已有的 LLVM 与 Windows 资源编译器。`-Task environment` 可显示最终选用的工具路径。

## 项目缓存与临时目录

在仓库根目录打开 PowerShell 后，先为当前进程及其子进程设置项目缓存：

```powershell
.\scripts\Use-ProjectCache.ps1 -Report
```

项目下载的构建依赖、下载缓存和临时文件均放在项目所在磁盘。本机仓库位于 E 盘，因此这些项目数据不会默认写入 C 盘用户缓存。配置只作用于当前进程，不修改全局用户环境。

| 内容 | 项目内位置 |
| --- | --- |
| Cargo 注册表、下载与 Git 依赖 | `.devtools/cargo` |
| Rust 工具链 | `.devtools/rustup` |
| Rust 编译产物 | `src-tauri/target` |
| pnpm store、元数据缓存、状态 | `.devtools/pnpm-store`、`.devtools/pnpm-cache`、`.devtools/pnpm-state` |
| npm、Corepack、Python 等工具缓存 | `.devtools` 下的对应目录 |
| TEMP / TMP | `.devtools/tmp` |

`Invoke-Rust.ps1`、`Verify.ps1` 和 `Prepare-Release.ps1` 会自行加载此配置；直接运行包管理器或其他构建工具时，应先执行上面的命令。pnpm 11 使用 `pnpm-workspace.yaml` 中的 `storeDir/cacheDir/stateDir`，并由 `.npmrc` 和进程环境提供一致配置，不能仅依赖旧版 `.npmrc` 的 store 设置。项目缓存目录和构建产物均已忽略，不提交到 Git。

## 安装前端依赖

在仓库根目录执行：

```powershell
.\scripts\Use-ProjectCache.ps1
pnpm install --frozen-lockfile
```

依赖安装复用项目缓存，`--frozen-lockfile` 保证使用仓库记录的依赖版本。

品牌 SVG 和托盘 PNG 已包含在源码中。如更新 `simple-icons`、Java SVG或图标映射，可重新生成托盘资源：

```powershell
pnpm icons:generate
```

该命令只写入项目的 `src-tauri\icons\menu`。

## 验证

```powershell
.\scripts\Verify.ps1
```

该脚本依次执行：

1. `tsc --noEmit`
2. `vitest run`
3. `vite build`
4. `cargo fmt -- --check`
5. `cargo test`
6. `cargo clippy --all-targets -- -D warnings`

需要显式联网的测试默认忽略，单独执行：

```powershell
.\scripts\Invoke-Rust.ps1 -Task live-sources
.\scripts\Invoke-Rust.ps1 -Task live-added-sources
.\scripts\Invoke-Rust.ps1 -Task live-install
```

`live-added-sources` 单独验证 Maven、.NET SDK、Ruby、PHP，便于区分新增解析器与其他上游网络波动。`live-install` 会在仓库的 `artifacts/integration` 下创建临时目录，真实下载 Python 官方嵌入包并完整验证安装事务；结束后删除临时目录，不修改 PATH。

## 开发运行

标准 SDK 完整的机器可执行：

```powershell
.\scripts\Use-ProjectCache.ps1
pnpm tauri dev
```

当前开发机应在已加载项目 SDK 环境的终端运行，或先用脚本完成 `check/test`。开发态数据默认位于仓库 `.envnexus-ai-data`；如果只存在旧版 `.envpilot-data`，新版本会继续读取它。

## Release 与 NSIS

```powershell
.\scripts\Invoke-Rust.ps1 -Task tauri
```

输出：

```text
src-tauri/target/release/envnexus-ai.exe
src-tauri/target/release/bundle/nsis/EnvNexus AI_0.1.5_x64-setup.exe
src-tauri/target/release/bundle/nsis/EnvNexus AI_0.1.5_x64-setup.exe.sig
```

`envnexus-ai.exe` 无参数时启动 GUI，有命令参数时作为工具脚本的内部命令引擎。便携交付把主程序和安装包复制到 `release/`，并附 `SHA256SUMS.txt`。

`createUpdaterArtifacts` 已启用，因此本地签名打包需要 updater 私钥。开发机默认从未提交的 `.devtools\updater\envnexus-ai.key` 读取；其他受信任的本地构建环境可通过 `TAURI_SIGNING_PRIVATE_KEY` 提供同一长期私钥。不持有私钥的 CI 按下述未签名流程构建。私钥一旦遗失，已安装版本将无法验证以后使用其他密钥签名的更新包。不得把私钥、密码或 API key 提交到仓库。

静态 GitHub updater 还需要在 Release 中上传 `latest.json`、安装包、便携包及各自同名 `.sig`。`latest.json` 的 `platforms.windows-x86_64` 描述安装包，`portable.windows-x86_64` 描述便携包；两者都必须包含 Release URL、完整 minisign 签名和 SHA-256。

标准本地构建完成后，准备发布说明并生成最终发布文件：

```powershell
.\scripts\Prepare-Release.ps1 -ReleaseNotesFile .\artifacts\release-notes.md
.\scripts\Invoke-Rust.ps1 -Task release-assets
```

`Prepare-Release.ps1` 检查主程序和 NSIS 安装包版本，生成便携包签名、更新清单及 `SHA256SUMS.txt`。`release-assets` 使用应用更新器的内置公钥验证两份实际发行文件的签名和清单哈希。

## GitHub Actions 发布验证

`.github/workflows/verify-release.yml` 在 `main`、`codex/**` 分支推送、非草稿 PR 和手动触发时运行 Windows 校验与构建。它执行前端和 Rust 检查、命令脚本冒烟、NSIS 隔离安装，并上传安装包、便携包和日志。分支推送及手动运行还执行官方源查询、Python 安装事务和全部 15 类工具的隔离安装检查；每项完成后清理本次测试的大型包，具体通过结果以工作流和 `TEST_REPORT.md` 为准。

工作流把项目构建缓存、Cargo/Rustup 和临时文件放在仓库目录；pnpm 启动工具安装在工作区相邻的隔离目录，避免被项目依赖安装递归扫描。项目缓存由 Actions cache 保存，构建文件通过 artifact 交付。

没有预先配置的签名 secret 时，工作流关闭 `createUpdaterArtifacts`，只生成未签名的验证包和哈希，不生成可供更新器使用的 `latest.json`。维护者可在本机使用原长期密钥对 CI 产物签名并生成发布清单，私钥不需要上传云端；已有签名 secret 的分支构建可使用工作流中的签名分支。

签名资产应先上传到对应版本的 draft Release，再运行 `verify-signed-release.yml`：

```powershell
gh workflow run verify-signed-release.yml --ref main -f tag=v0.1.5
```

所选源码版本必须与 tag 一致。该工作流仅需 `contents:read`，使用仓库令牌下载六份公开发行资产，不接触私钥；它检查 PE 版本、发布 URL、签名旁文件、完整 SHA-256 清单，并运行应用自身的 Rust 签名验证测试。验证报告随运行上传，发布完成应以实际验证结果为依据。首次启用手动触发前，该工作流文件需要存在于默认分支。

命令脚本隔离冒烟：

```powershell
.\scripts\Smoke-Cli.ps1
```

该脚本使用单独的 `ENVNEXUS_AI_DATA_ROOT`，生成 110 个工具命令脚本并实际执行 `jdk-list.cmd`、`env-refresh.cmd` 与 `env-repair.cmd`，验证无快照只读查询、15 个工具定义、先生成脚本后修改工具目录仍读取最新配置、完整扫描、增量刷新、共享快照和无 `--yes` 的修复预览，不执行真实环境变更。旧的 `ENVPILOT_DATA_ROOT` 仍作为只读兼容入口。桌面冒烟另会输入并保存自定义命令脚本目录，再确认脚本生成在该目录中，并打开本地诊断建议弹窗核对可复制命令。

窗口与托盘隔离冒烟：

```powershell
.\scripts\Smoke-Tray.ps1
```

该脚本使用隔离数据目录、真实 Windows 窗口消息和 Tauri 托盘事件，验证初始化期间打开 Python 能直接进入独立管理页、设置持久化、关闭按钮到托盘、立即隐藏、启动后隐藏、直接退出、五种语言切换、15 个工具的托盘层级，以及 Windows 当前用户开机自启项的启用和停用。脚本会备份并在结束时恢复原有 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\EnvNexus AI` 值；上述启动过程不会创建扫描快照。

NSIS 使用 `currentUser` 安装模式和 Tauri 默认安装器模板，并配置 WebView2 download bootstrapper。默认模板保留安装目录选择页；如果注册表中存在同一应用之前保存的安装目录，安装器会恢复该目录并在原位置覆盖升级。应用内更新由独立帮助进程以 `/S /UPDATE` 静默运行安装器；便携版则在主程序退出后原路径替换。两种模式都会先保留旧主程序，等待新版本完成启动确认后才清理，失败时自动恢复。安装目录恢复由 Tauri 默认 NSIS 模板的 `RestorePreviousInstallLocation` 实现：
[Tauri Windows Installer 文档](https://v2.tauri.app/distribute/windows-installer/)

Tauri updater 的 minisign 签名只用于更新包完整性与来源校验，不等同于 Windows Authenticode；正式分发前仍建议配置 Windows 代码签名证书。

`Prepare-Portable.mjs` 在签名前为便携副本写入与 Tauri NSIS 相同的 bundle 标记，保留编译输出原件。CI 静默安装 NSIS 后逐字节校验主程序与发布便携包的 SHA-256 一致，以兼容旧更新器的目标校验。脚本会拒绝已含 Authenticode 签名的输入；将来添加代码签名时，必须在签名前处理标记并继续保证两种交付方式使用相同的已签名主程序。

## 构建环境警告

当前项目内 xwin SDK 的 MSVC 静态运行库不包含调试 PDB，Release 链接会显示 `LNK4099`。链接成功且二进制可启动；该警告只表示无法为系统运行库对象加载调试符号。使用完整 Windows SDK/Visual Studio 安装时通常不会出现。
