# 构建说明

## 支持平台

- Windows 10/11 x64；
- Rust MSVC target；
- Tauri 2 / WebView2。

## 标准前置条件

1. Visual Studio 2022，安装“使用 C++ 的桌面开发”工作负载；
2. Windows 10 或 Windows 11 SDK；
3. Rust stable，至少满足 `rust-version = 1.85`；
4. Node.js 20.19+ 或 22.12+；
5. pnpm 11。

当前开发机的系统 Windows SDK 注册路径失效。为避免修改系统配置，本工作区把 Rust 1.97.1 和缺失 SDK 头文件/库放在 `.devtools`；`scripts/Invoke-Rust.ps1` 会优先使用这些项目内依赖，并从 Visual Studio NuGet SDK Build Tools 找到 `RC.EXE`。

## 安装前端依赖

在仓库根目录执行：

```powershell
pnpm install
```

`.npmrc` 将 pnpm store 放在项目 `.devtools/pnpm-store`，不会把本项目缓存默认写到 C 盘。

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
4. `cargo fmt`
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
src-tauri/target/release/bundle/nsis/EnvNexus AI_0.1.2_x64-setup.exe
src-tauri/target/release/bundle/nsis/EnvNexus AI_0.1.2_x64-setup.exe.sig
```

`envnexus-ai.exe` 无参数时启动 GUI，有命令参数时作为工具脚本的内部命令引擎。便携交付把主程序和安装包复制到 `release/`，并附 `SHA256SUMS.txt`。

`createUpdaterArtifacts` 已启用，因此 Tauri 打包需要 updater 私钥。开发机默认从未提交的 `.devtools\updater\envnexus-ai.key` 读取；其他机器和 CI 应通过 `TAURI_SIGNING_PRIVATE_KEY` 提供同一长期私钥。私钥一旦遗失，已安装版本将无法验证以后使用其他密钥签名的更新包。不得把私钥、密码或 API key 提交到仓库。

静态 GitHub updater 还需要在 Release 中上传 `latest.json`、安装包和同名 `.sig`。`latest.json` 的 `windows-x86_64.url` 必须指向该 Release 的安装包，`signature` 必须是 `.sig` 文件的完整内容。

命令脚本隔离冒烟：

```powershell
.\scripts\Smoke-Cli.ps1
```

该脚本使用单独的 `ENVNEXUS_AI_DATA_ROOT`，生成 109 个工具命令脚本并实际执行 `jdk-list.cmd` 与 `env-repair.cmd`，验证无快照只读查询、15 个工具定义、先生成脚本后修改工具目录仍读取最新配置、显式扫描、共享快照和无 `--yes` 的修复预览，不执行真实环境变更。旧的 `ENVPILOT_DATA_ROOT` 仍作为只读兼容入口。桌面冒烟另会输入并保存自定义命令脚本目录，再确认脚本生成在该目录中，并打开本地诊断建议弹窗核对可复制命令。

窗口与托盘隔离冒烟：

```powershell
.\scripts\Smoke-Tray.ps1
```

该脚本使用隔离数据目录、真实 Windows 窗口消息和 Tauri 托盘事件，验证初始化期间打开 Python 能直接进入独立管理页、设置持久化、关闭按钮到托盘、立即隐藏、启动后隐藏、直接退出、五种语言切换、15 个工具的托盘层级，以及 Windows 当前用户开机自启项的启用和停用。脚本会备份并在结束时恢复原有 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\EnvNexus AI` 值；上述启动过程不会创建扫描快照。

NSIS 使用 `currentUser` 安装模式和 Tauri 默认安装器模板，并配置 WebView2 download bootstrapper。默认模板保留安装目录选择页；如果注册表中存在同一应用之前保存的安装目录，安装器会恢复该目录并在原位置覆盖升级。应用内更新使用 `passive` 模式，因此不重复询问目录并沿用现有安装位置。对应行为由 Tauri 默认 NSIS 模板的目录页与 `RestorePreviousInstallLocation` 实现：
https://v2.tauri.app/distribute/windows-installer/

Tauri updater 的 minisign 签名只用于更新包完整性与来源校验，不等同于 Windows Authenticode；正式分发前仍建议配置 Windows 代码签名证书。

## 构建环境警告

当前项目内 xwin SDK 的 MSVC 静态运行库不包含调试 PDB，Release 链接会显示 `LNK4099`。链接成功且二进制可启动；该警告只表示无法为系统运行库对象加载调试符号。使用完整 Windows SDK/Visual Studio 安装时通常不会出现。
