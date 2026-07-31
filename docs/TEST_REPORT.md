# EnvNexus AI 0.1.4 测试报告

测试日期：2026-07-31
系统：Windows 11 Home China `10.0.26200` x64  
构建工具链：Rust `1.97.1`、Tauri `2.11.5`、Node.js `25.2.1`、pnpm `11.9.0`

## 自动测试

| 项目 | 命令 | 结果 |
| --- | --- | --- |
| TypeScript | `tsc --noEmit` | 通过 |
| 前端单元测试 | `vitest run` | 41/41 通过（14 个测试文件） |
| 前端生产构建 | `vite build` | 通过，1792 modules |
| Rust 格式化 | `cargo fmt` | 通过 |
| Rust 单元测试 | `cargo test` | 59 通过，4 个发布产物或联网测试按设计忽略 |
| Rust 静态检查 | `cargo clippy --all-targets -- -D warnings` | 通过 |
| 15 官方源联网复测 | `Invoke-Rust.ps1 -Task live-sources` | 未通过：`go.dev` 两次连接超时；未伪报通过 |
| 新增四源联网测试 | `Invoke-Rust.ps1 -Task live-added-sources` | 1/1 通过；Maven 1、.NET 18、Ruby 24、PHP 6 个 Windows 下载 |
| Python 实际安装事务 | `Invoke-Rust.ps1 -Task live-install` | 通过 |

## 2026-07-31 全机扫描与性能复测

- 完整扫描遍历本机 C、D、E、F 四个固定磁盘，索引覆盖 15 类工具；测试快照包含 161 个已验证安装版本。
- 同一全机索引首次执行增量探测用时 33.188 秒；生成 `tool-version-probes.json` 后，两次缓存刷新分别用时 2.478 秒和 2.379 秒。
- 首次与缓存刷新均返回 15 类工具、161 个安装版本，数量一致；缓存刷新仍实时重新验证当前 PATH 默认工具。
- 指纹包含可执行文件大小、修改时间，以及 Android NDK 的 `source.properties` 元数据；文件发生变化后缓存测试确认会拒绝复用并重新执行版本命令。
- 空闲界面没有定时扫描或轮询。Windows WebView2 在窗口失焦、隐藏或进入托盘时切换到低内存目标，恢复窗口后切回正常目标；后台 Rust 下载与更新任务不暂停。
- 在相同的全新隔离数据目录、窗口最小化并失焦 30 秒后，对整个 7 进程树连续采样 8 秒：0.1.3 平均工作集 436.1 MiB，0.1.4 平均工作集 101.6 MiB、峰值 102.8 MiB，物理驻留工作集下降约 76.7%。
- 同一采样窗口内，两版均未观察到可计量的空闲 CPU 增长。0.1.4 平均私有提交量为 401.2 MiB，与 0.1.3 的 400.1 MiB 基本一致，因此不将此次优化表述为虚拟提交量下降。
- 最终便携包的 CLI 冒烟通过：全机扫描、增量刷新、15 类工具、110 个脚本、JDK 别名、根目录动态读取和无确认令牌的预览保护均成功。托盘冒烟通过关闭到托盘、页面恢复、启动隐藏、开机启动、语言切换和正常退出。

## 2026-07-24 EnvNexus AI 改名与更新器复测

- `.\scripts\Verify.ps1 -Release` 完整通过：TypeScript、Rust fmt、38 项 Rust 测试和严格 Clippy 均成功；3 项显式联网测试按设计忽略。随后针对设置页修复新增测试，TypeScript、19 项 Vitest 与 Vite 生产构建再次通过。
- Tauri 生成 `envnexus-ai.exe` 和 current-user NSIS 安装包；安装包使用项目长期 updater 密钥生成 424 字节 minisign 签名。
- `latest.json` 已做结构校验，平台键为 `windows-x86_64`，下载 URL 指向 `v0.1.0` GitHub Release，签名字段非空。
- `scripts\Smoke-Cli.ps1` 对改名后的 Release 主程序通过：15 个工具、110 个脚本、目录持久化、完整扫描、增量刷新、快照复用和无 `--yes` 的预览保护均成功。
- 用户确认旧窗口来源后，改名后的独立 GUI 复测完成：单实例恢复、托盘层级、关闭到托盘、启动后隐藏、语言切换、手动扫描、上次页面恢复均通过。
- 设置页真实 WebView 复测确认两个操作按钮使用相同高度和垂直中心；滚动到 AI 厂商区域后点击会触发整页重绘的厂商按钮，重绘前后主内容 `scrollTop` 差值不超过 2 像素，未跳回顶部。
- 构建仍出现项目内 xwin SDK 缺少静态运行库 PDB 的 `LNK4099` 警告；链接、测试和安装包生成成功，此警告不影响运行，但也未被隐藏。

Rust 单元测试覆盖：

- PATH 重复、相对、缺失、空项诊断；
- `RUST1`/`RUST2` 等非标准工具别名指向不同绝对目录时的保守冲突诊断；
- 同一工具由多个版本管理器接管，以及管理器根目录与外部安装并存诊断；
- 本地诊断 guidance 的原因、本机因素、命令和“自定义别名不自动写入”安全边界；
- 环境快照指纹稳定性；
- 扫描候选去重、安装根推导，以及用户保存的工具根目录发现；
- SHA-512/SHA-256/SHA-1；
- 安全下载文件名和安装根边界；
- Android repository XML/Windows archive/SHA-1 解析；
- PATH 切换时旧版本清理；
- EnvNexus AI 受管 Rust 的隔离 HOME；
- Android SDK/ADB 共享根目录激活；
- 工具根目录配置写入、重新读取，以及 Android 六工具共享根目录同步；
- 卸载时只清理该受管版本的环境引用；
- 扫描快照写入/读取，读取缓存路径不会触发新扫描；
- 用户 PATH 重复、失效条目的修复计划，以及版本管理器根目录保护；
- 用户级/系统级同名变量仅在值相同时允许删除重复用户变量；
- OpenAI Compatible 与 Gemini 模型列表解析；
- 内置九种 AI 厂商配置合并、非 HTTPS URL 拒绝；
- 九种 AI 厂商品牌 SVG 映射和无字母占位图形断言；
- 各 AI 厂商配置文件独立写入、旧聚合配置迁移、当前厂商显式切换；
- 多厂商 Windows DPAPI API Key 独立加密/解密及单独删除。
- 工具命令别名解析、110 个 CMD 脚本生成、自定义命令目录持久化，以及 `--yes` 确认标志只移除自身、不改变其他参数。
- 应用行为默认值、五种语言、开机自启设置和 `<data-root>\config\app-preferences.json` 持久化往返。
- Windows 当前用户 `Run` 启动命令的引号规则。

## 实时官方源

同一联网测试实际请求并解析：

```text
python, java, go, rust, node, git, maven, dotnet, ruby, php,
android-sdk, android-ndk, gradle, cmake, adb
```

测试要求每个 catalog 非空，且至少包含一个 HTTPS Windows 下载 URL。2026-07-23 的完整聚合复测运行两次，均在 `https://go.dev/dl/?mode=json&include=all` 连接超时后失败（约 81 秒与 115 秒）；这项结果记录为失败。新增四源独立复测通过，用时 17.90 秒：Maven 1、.NET SDK 18、Ruby 24、PHP 6。

## 实际下载、校验和安装

实际使用：

```text
Python 3.13.14
https://www.python.org/ftp/python/3.13.14/python-3.13.14-embed-amd64.zip
SHA-256 90b4e5b9898b72d744650524bff92377c367f44bd5fbd09e3148656c080ad907
```

测试在仓库 `artifacts/integration` 的临时目录内完成：

```text
官方源查询 -> 下载 -> SHA-256 -> ZIP 安全解压
-> 暂存提交 -> 安装清单 -> python.exe --version -> committed
```

结果：通过，用时约 11.60 秒。临时目录随后删除；没有修改 PATH、注册表或现有 Python。

## Release 构建与打包

Tauri Release 和 NSIS 均已成功：

| 文件 | 大小 | SHA-256 |
| --- | ---: | --- |
| `EnvNexus-AI_0.1.4_x64-portable.exe` | 10,848,768 bytes（10.35 MiB） | `9884bbcd0c9876799071e1de505eb484d9c2d92b2ad21e2e8b166c3c05da8f32` |
| `EnvNexus-AI_0.1.4_x64-setup.exe` | 5,181,314 bytes（4.94 MiB） | `3a555abe72cfc46617824048b28a57fd0cad8d7a7fb8ece1abfe26f9cf3a56e7` |

校验文件：`release/SHA256SUMS.txt`。

对最终复制到 `release` 的 portable 文件再次执行单实例与命令模式冒烟：第二个 GUI 进程退出码为 0，原进程保持运行并恢复隐藏窗口；`tools --json` 返回 15 个工具，隔离数据目录中没有生成扫描快照。两份新 Release 文件的重新计算哈希均与 `SHA256SUMS.txt` 匹配。

当前开发机链接时出现 `LNK4099`：项目内 xwin SDK 静态运行库没有调试 PDB。链接仍成功，Release 可启动；该警告已记录，不解释为测试失败。前端构建还提示主 JavaScript chunk 为 531.77 kB，超过默认 500 kB 警告阈值；gzip 后为 137.11 kB，构建成功，后续可通过页面级拆包继续优化。

## 运行冒烟

历史完整 GUI 冒烟使用：

```text
scripts\Smoke-ManualScan.ps1
ENVNEXUS_AI_DATA_ROOT=<repo>\artifacts\smoke\manual-scan-4ef45dfc04c846bf841a03247e9a1abc\data
```

实测结果：

- 进程未退出；
- 主窗口句柄有效；
- 标题为 `EnvNexus AI`；
- 全新数据目录启动后仍没有 `last-environment-scan.json`，确认启动未扫描；
- 未扫描状态下通过 WebView DOM 断言工具链显示 9 个常用工具和 6 个 Android 工具、Android 共享目录输入，且不存在单独 Android 顶层导航；
- 未扫描状态下打开 Python 独立页，填写并保存隔离测试目录，`tool-roots.json` 持久化成功；
- 通过 WebView CDP 触发真实“开始扫描”按钮后生成扫描快照；
- 快照包含 15 个工具；
- 识别 pyenv-win、NVM for Windows、rustup 三个版本管理器；
- 关闭并重新启动 App 后，扫描快照 SHA-256 和 `LastWriteTimeUtc` 均未改变，确认重启只复用上次结果；
- 点击“本地分析与建议”后成功打开本地诊断弹窗，显示原因证据、本机适配因素、修复建议与 3 条可复制命令；
- 点击首个可修复诊断后成功打开用户级环境修复计划，显示 HKCU 范围、PATH 差异、版本管理器路径保护、备份与回滚说明；
- “命令说明”位于“设置”上方，显示 15 个工具命令分组；输入并持久化自定义命令目录后，110 个 CMD 脚本生成在该目录，并打开用户 PATH 差异计划；随后取消，未修改 HKCU；
- 设置页实际显示九个带品牌图形的 AI 厂商入口、URL、协议、API Key、远程获取模型、模型选择和“设为当前 AI”操作；
- 使用隔离数据目录和不联网的虚拟值分别保存 OpenAI 与 DeepSeek，确认生成 `providers\openai.json`、`providers\deepseek.json`、`secrets\openai.dpapi.json` 和 `secrets\deepseek.dpapi.json` 四个独立文件；
- 保存 DeepSeek 后 OpenAI 配置文件字节和已选模型不变；当前厂商必须显式切换，托盘状态只列出两项有效配置，并能从 OpenAI 切换到 DeepSeek；
- 设置页应用行为区域为紧凑两列布局，实际显示关闭按钮行为、五种语言、登录后自启动、启动后隐藏、托盘状态和立即隐藏操作；已确认原“最小化按钮也隐藏到托盘”选项不存在；
- 现代科技主题的总览、工具链、Python 独立管理页、设置页和诊断修复弹窗均完成截图检查；深海军蓝网格、薄荷青 HUD 线框、切角面板、环形健康仪表及主题预览均生效，内容没有被裁切；
- 现代科技下九个 AI 品牌图形均显示圆形 HUD 光环；切换为赛博朋克后同一图形不再是圆形，确认圆形规则没有泄漏到其他主题；
- 游戏 HUD 实际显示黑色底、橙色霓虹、重复蜂窝描边、六边形健康度仪表、切角面板和发光按钮；计算样式确认蜂窝 SVG、面板和图标多边形均已生效；
- 退出前停留在 Java/JDK 详情页；重新启动后直接恢复该详情页，同时扫描快照 SHA-256 与时间戳保持不变；
- 工具链卡片实际显示 15 个品牌 SVG；Python、Go、Rust、Node.js、Git、Maven、.NET、Ruby、PHP 截图人工检查通过，Java 多色咖啡杯 SVG 由单元测试和生成后的 PNG 解码覆盖；
- 手动扫描后托盘状态返回 15 个工具层级、35 个非默认已安装版本切换入口、18 个诊断问题和 13 个可修复诊断入口，数量均与缓存快照一致；
- 通过托盘事件打开首个诊断的本地详情，并通过托盘修复事件打开对应环境修复计划；
- 未执行任何环境修改。

窗口与托盘另使用 `scripts\Smoke-Tray.ps1` 和隔离数据目录：

```text
artifacts\smoke\tray-5114d1da707b4fc891bbc80e07e774b8
```

实测结果：

- Windows 托盘图标创建成功；
- 托盘菜单状态为 ready，带品牌图标的 15 个工具层级全部构造成功；
- App 初始化阶段立即派发托盘 `openTool=python` 后，成功进入 Python 独立管理页并显示目录输入框，没有出现“工具信息不可用”，也没有创建扫描快照；
- 关闭按钮设置为“最小化到托盘”后窗口隐藏且进程继续运行；
- 关闭到托盘和点击“隐藏到托盘”后重新恢复窗口，均仍停留在原设置页，没有被强制切回总览；
- 启动后隐藏设置重启后生效，WebView 和托盘仍可恢复；
- 开机自启启用时写入带引号的当前主程序路径，停用后删除同名值；测试前存在的值由脚本备份并在结束时恢复；
- 保存 `en-US` 后设置页导航切换为 English，保存 `ja-JP` 后配置正确持久化，最后重置为简体中文；
- 关闭行为改回“直接退出”后进程正常退出；
- 全部启动和恢复步骤均未生成扫描快照。

GUI 单实例另使用 `scripts\Smoke-SingleInstance.ps1`，并对最终 `portable` 文件复测：

```text
artifacts\smoke\single-instance-41d22d25fa144cf78babb9ef79532c50
```

实测结果：

- 第一个 GUI 进程保持运行；
- 第二次启动的 GUI 进程在 10 秒限制内退出，退出码为 0；
- 测试先隐藏原窗口，第二次启动后原窗口重新可见；
- GUI 运行期间执行 `tools --json` 仍返回 15 个工具；
- 没有生成扫描快照。

单一 `EnvNexus-AI.exe` 的命令入口另使用 `scripts\Smoke-Cli.ps1` 和隔离数据目录：

```text
artifacts\smoke\cli-c9fc0587c08742c5909d51cab1d856be
```

实测结果：

- `tools --json` 返回 15 个内置工具，且未生成扫描快照；
- 生成 110 个 CMD 脚本，实际执行 `jdk-list.cmd --json` 正确映射到 `java`，且无快照时未触发扫描；
- `root set python <absolute-path>` 持久化到共享 `tool-roots.json`；
- 在脚本生成之后修改 Python 根目录，原先生成的 `python-root.cmd get --json` 返回最新目录，确认脚本没有写死工具路径；
- 显式 `scan` 后生成共享快照，随后 `jdk-list` 复用其中的扫描时间和 Java/JDK 清单；
- 选择一个可修复诊断执行 `diagnostic-repair <code> --json`，未附 `--yes`，只返回带确认令牌的计划；HKCU 用户环境前后完全一致。
- 实际执行生成的 `env-repair.cmd` 预览同一修复计划，并再次确认 HKCU 用户环境未改变。

工具界面与滚轮另外使用 `scripts/Smoke-ToolPages.ps1` 实测：

- “工具链”总览显示 9 个常用工具和 6 个 Android 构建工具，侧边栏没有独立 Android 模块；
- 自动点击 Python 卡片后进入完整独立管理页；
- 独立页明确显示“Python 默认安装根目录”和“选择目录”入口；
- 向内容区域发送 8 次真实鼠标滚轮事件后，页面从顶部滚动到后续本机版本；
- 滚动前后截图 SHA-256 不同，且人工检查滚动条与可见版本内容均发生对应变化。
- 本次滚动前哈希 `80392FDD...AAD67`，滚动后 `49646224...624F7`。

截图：

- `artifacts/smoke/envpilot-tools.png`
- `artifacts/smoke/envpilot-tool-detail.png`
- `artifacts/smoke/envpilot-tool-detail-scrolled.png`
- `artifacts/smoke/manual-scan-4ef45dfc04c846bf841a03247e9a1abc/10-ai-provider-config.png`
- `artifacts/smoke/manual-scan-4ef45dfc04c846bf841a03247e9a1abc/11-game-hud-dashboard.png`
- `artifacts/smoke/manual-scan-4ef45dfc04c846bf841a03247e9a1abc/05-restart-reuses-snapshot.png`

## 未执行的高影响测试

- 没有修改现有 HKCU/HKLM/PATH；
- 没有申请管理员权限；
- 没有卸载或迁移现有工具；
- 没有在开发机上逐一实际安装 Java、Go、Rust、Node、Git、Android、Gradle、CMake；
- 没有使用真实厂商 API Key 请求 AI 模型列表或执行推理；不会伪造这类联网结果；
- 没有安装生成的 NSIS 包，以免向系统安装目录、卸载项写入；已验证 NSIS 生成与哈希。

这些未执行项不会被描述为已验证，详见 `KNOWN_LIMITATIONS.md`。
