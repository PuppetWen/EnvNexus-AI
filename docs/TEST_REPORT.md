# EnvNexus AI 测试报告

## 0.1.7 验证记录（2026-10-17 UTC）

本节以提交 `ba2c54e48bdb9a5606e8ccb8f6a29273c06795fa` 的 [CI7 / 39025514788](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/39025514788) 原始报告为依据。整轮工作流已确认 `COMPLETED / SUCCESS`，完成时间为 `2026-10-17T11:03:39Z`，包含功能、构建、报告上传和缓存收尾。原始报告下载目录为 `artifacts/history-ci-seventh-reports-r1/`；下文保留全部旧版记录，不将旧结果作为本版复测。

### 本轮主要结果

| 项目 | CI7 原始证据与结果 |
| --- | --- |
| 自动检查 | `ci-logs/verification.log`：62 项前端测试、4 项发布脚本测试、184 项 Rust 测试通过；6 项联网/官方包/发布产物测试在默认单元测试中显式忽略。TypeScript、Vite 生产构建、Rust 格式及 clippy 检查通过 |
| 官方源与安装事务 | `ci-logs/live-sources.log`：15 类官方源实时查询通过；`live-install.log`：官方完整 Python `3.15.0` 实际安装事务通过 |
| Python 历史目录 | 662 条记录、367 条可安装下载、295 条无下载记录；其中预览 376 条、归档频道 7 条，不可安装预览 189 条。关键词/频道组合、可安装筛选、版本及发布日期双向排序均通过 |
| 历史版本实际安装 | `python-history-e737b5d949f444608a4851c86b79e11a/summary.json`：10/10 通过，`complete: true`、`cleanup: passed`、`errors: []` |
| 现代完整包新装与修复 | `python-88e5773f1ac4460c86d919a01aacf499/summary.json`：`3.15.0` 新装、真实旧嵌入式安装的同版本原路径修复均通过，旧缓存被拒绝，pip/开发组件/venv 验收通过 |
| 命令模式 | `smoke/cli-a5e8525a5d864068bece7044d443afad/summary.json`：15 类定义、110 个生成命令、完成全盘扫描、根目录持久化、旧脚本读取最新根、只读不扫描及预览保护均通过 |
| 15 类工具安装 | `installability-7121709ad768441b84593a487738b05e/summary.json`：15/15 通过、0 失败，安装清单、切换预览、缓存与清理均通过；ADB 实际激活、默认命令回读和用户环境恢复通过 |
| 构建与 NSIS | `ci-logs/bundle.log`、`installer-payload.json`：0.1.7 构建及 NSIS 隔离安装通过，安装后主程序与便携包 SHA-256 相同 |

历史目录只刷新一次，后续筛选使用同一缓存，`fetchedAt` 保持 `2026-10-17T10:44:35.433393900Z`，缓存 SHA-256 为 `c51271c9ac1993b5966d7dc0c89be86c565daedfec8854fd4b617980cb5e3d85`。原始 `reports/catalog/validation.json` 确认 `filteredCommandsUsedUnchangedCache: true`。目录包含 `0.9.1`、`1.6.1` 和 `3.4.0a1/a4/b1/rc1` 等样本；缺少可校验 Windows 下载元数据的条目保留官方页和明确不可安装原因，源码标签不被当作 Windows 包存在或不存在的证明。

### 十个历史 Python 场景

每项均通过正式 CLI 下载官方原包、校验并静态提取，实际解释器版本、prefix、可执行文件和标准库检查通过；受管清单与中央清单的版本、来源 URL、路径及摘要一致。证据位于上述历史报告的 `reports/<version>/`，各包实测 MD5 如下。

| 版本 | 官方原包形式 | pip 验收 | venv 验收 | 已核对的原包 MD5 |
| --- | --- | --- | --- | --- |
| `2.0.1` | Wise EXE | 发行版未随附 | 发行版未随附 | `5940b6eea8972136f8e365346f1b1313` |
| `2.1.3` | Wise EXE | 发行版未随附 | 发行版未随附 | `fc8020b2be17c098b69368543c5589a9` |
| `2.2.3` | Wise EXE | 发行版未随附 | 发行版未随附 | `d76e774a4169794ae0d7a8598478e69e` |
| `2.3.5` | Wise EXE | 发行版未随附 | 发行版未随附 | `ba6f9eb9da40ad23bc631a1f31149a01` |
| `2.4.4` | MSI/CAB | 发行版未随附 | 发行版未随附 | `8b1517fdbf287d402ac06cc809abfad6` |
| `2.7.18` | amd64 MSI/CAB | `19.2.3` 模块、列表、`pip.exe` | 发行版未随附 | `a425c758d38f8e28b56f4724b499239a` |
| `3.0.1` | amd64 MSI/CAB | 发行版未随附 | 发行版未随附 | `be8f57265e1419330965692a4fa15d9a` |
| `3.3.5` | amd64 MSI/CAB | 发行版未随附 | 独立解释器、prefix/base_prefix、小程序；不含 pip | `ebb3ab0df91389a6dd45317d6f4ac838` |
| `3.4.4` | amd64 MSI/CAB | `7.1.2` 模块、列表、`pip.exe` | 同版 pip 模块、列表、入口及小程序 | `963f67116935447fad73e09cc561c713` |
| `3.10.11` | amd64 Burn EXE 内 MSI/CAB | `23.0.1` 模块、列表、`pip.exe` | 同版 pip 模块、列表、入口及小程序 | `a55e9c1e6421c84a4bd8b4be41492f51` |

前九项实际安装于本轮 GUID 内 `versions/<version>/tools/python/<version>` 的 ASCII 路径。各项先以 `unsupported-开发-é` 根生成预览，均在安装开始前被拒绝，`incompatible-path.json` 记录 `installationStarted: false`；Python `<3.6` 应使用如 `E:\Environment` 的 ASCII 安装目录。`3.10.11` 实际安装于 `versions/3.10.11/tools-开发-é/python/3.10.11`，解释器、pip 模块/输出/入口及 venv 均通过。

十项 `setup-isolation.json` 均为 `passed`、`sentinelCount: 0`：故意注入的 `PIP_TARGET/PIP_PREFIX/PIP_ROOT/PIP_USER/PIP_LOG`、fake HOME 中 `pydistutils.cfg`、不可信 CWD 中 `setup.cfg` 以及 `platform/runpy/pip` 导入哨兵未影响安装。测试仅在 runner 本次 GUID 的 D 盘工作区安装和缓存，逐项及整轮用户环境保持 `unchanged`，清理通过；历史与现代 Python 冒烟执行前后 `HKCU\Environment` 指纹均为 `82be763dab7e32567aa983a6c3361d2762275e991a7cb2479fcbbbef7a158ba5`。历史包未运行厂商安装器、MSI 自定义动作或全局运行库安装程序。

现代 `3.15.0` 原包为 `python-3.15.0-amd64.zip`，45,140,897 字节，SHA-256 `6b1a3f885ce416cf3a68af203e62203664eb5224d8d0c1d2a1945fddec5c500b`。新装与修复均验证 pip `26.2.1`、Tk `9.0`、IDLE、`include/Python.h`、3 个导入库及含 pip 的 venv；修复来源由 `python-3.15.0-embed-amd64.zip` 变为完整 ZIP，`sameVersion`、`oldCatalogRejected` 均为 `true`，安装路径保持原值。

### 前六轮失败与后续修复

| 轮次 | 失败记录及处理 |
| --- | --- |
| CI1 / `95a825b` | 官方数据库重复 `3.3.5rc1` 导致目录查询失败；改为合并同版本候选，仍拒绝矛盾摘要 |
| CI2 / `28f2437` | 历史参数/flags、3.0 探针 CRLF、旧版非 ASCII 路径及筛选测试的扩展名假设失败；修正年代兼容探针、路径预检与完整 ZIP 筛选断言 |
| CI3 / `a48f49e` | Wise helper 的三处 `ZipError` 转换导致编译失败；补显式统一错误转换，未将此轮计为安装成功 |
| CI4 / `6f00b9b` | 历史归档内容无法识别，旧日志未给出具体页面，根因不作追认；补有界 gzip 解码、逐页验证、异常页重试及精确来源错误 |
| CI5 / `665da95` | 四个 Wise 场景的脚本/ZIP 配对失败，2.7.18 出现 `connectionpool` 导入失败，3.4.4 最终无 pip；修复 Wise 配对，改用私有展开的原包 wheel、显式 SSL 探测及传播 pip 退出状态。3.4.4 旧日志不能单独证明 SSL 是唯一根因 |
| CI6 | 单元测试通过后，三个测试用 `clone` 触发严格 clippy 失败；修正为引用切片。上述轮次均未计为整轮成功，实际历史安装结论以 CI7 十项报告为准 |

### 工具激活、产物和交付边界

15 类实际版本为：Python `3.15.0`、Java `27+35`、Go `1.27.2`、Rust `1.99.1`、Node.js `26.12.0`、Git `2.56.0.windows.2`、Android Command-line Tools `23.0`、NDK `30.0.16248370`、Gradle `9.8.1`、CMake `4.4.4`、ADB `37.0.1`、Maven `3.10.0`、.NET SDK `10.0.402`、Ruby `4.0.7-1`、PHP `8.5.11`。仅 ADB 实际应用用户环境计划并恢复，其余 14 类验证切换预览；安装完成仍需单独确认环境变更，系统 PATH 遮蔽保护继续生效。

`release/SHA256SUMS.txt` 与 CI7 `installer-payload.json` 中的主程序摘要一致；当前本地产物摘要如下，公开下载回读仍待完成。

| 资产 | SHA-256 |
| --- | --- |
| `EnvNexus-AI_0.1.7_x64-setup.exe` | `0f4ea8c1b51d20cff7a70c7c39111398cd65f88eb84d174d02d8d0cdc6a7e392` |
| `EnvNexus-AI_0.1.7_x64-portable.exe` / NSIS 安装后主程序 | `879b28b2d659fad121da7d077186f15ffca9675d1f5ce51e340d9f4ccb8dfb4c` |
| `latest.json` | `45dc83ddf69ace74a73439ff5864c683976c8da570b7a42130ca82fecb98d982` |

独立签名验证 [39027005455](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/39027005455) 已以 `COMPLETED / SUCCESS` 完成，完成时间为 `2026-10-17T11:06:57Z`；六项资产下载、版本/URL/完整 SHA-256 清单检查、应用内公钥的真实 Rust updater 签名验证及报告上传和缓存收尾均通过。正式公开发布、最终合并提交/标签对应关系及无凭据公开六资产回读仍待确认。

以上十个稳定历史版本通过不表示全部 662 条版本、所有预览版组件、第三方包、原生扩展编译或 IDE 集成都已验证；pip/venv 按原发行版实际能力验收，没有为旧版补装现代 pip 或 virtualenv。历史包使用的官方 MD5 属于弱校验，不能等同于 SHA-256 或发行签名。此轮没有在用户开发机执行 Python PATH 切换或绕过 AppControl 启动新 GUI；Android SDK 覆盖 Command-line Tools，不含平台镜像或模拟器。

## 0.1.6 验证记录（2026-10-15 UTC）

本节记录完整 Python 开发运行时修复的当前证据。0.1.5 的测试结果保留在下一节，不作为 0.1.6 完整 Python、旧安装迁移或最终发布包已通过的证明。

### 本机官方包与索引检查

- 实读 Python 官方运行时索引三页：`index-windows.json` → `index-windows-recent.json` → `index-windows-legacy.json`。按当前筛选条件得到 47 个稳定 Windows x64 完整 ZIP，范围为 `3.11.0` 至 `3.15.0`。
- 实际字段核对覆盖 `3.14.3`、`3.12.9`、`3.11.0`；正常完整包的 `id/tag` 均为不带 patch 的 `pythoncore-3.<minor>-64` / `3.<minor>-64`。27 个明确带 `t` 的 free-threaded 条目被排除，正常完整版本误过滤数为 0。旧 NuGet 包及无有效官方 SHA-256 的条目不会作为完整运行时替代品。
- 官方 `python-3.15.0-amd64.zip` 大小为 `45,140,897` 字节，SHA-256 为 `6b1a3f885ce416cf3a68af203e62203664eb5224d8d0c1d2a1945fddec5c500b`，与官方清单一致。
- 在本项目 E 盘隔离目录 `.devtools/python-full-smoke-3f16062fbc2d4551aa60a7bc8c92b7f1` 中，普通 `python.exe -I -m pip` 检查、离线生成 `pip.exe`、创建 venv 及 venv 中的 `pip list` 均通过；pip 版本为 `26.2.1`。没有修改用户已有 Python 安装或全局环境。
- 安装器实际使用的 `runpy` wrapper 也已在 E 盘私有隔离目录中验证通过：预先设置 `ENSUREPIP_OPTIONS=altinstall`，子进程移除此值，并使用 `runpy.run_module(module, run_name='__main__', alter_sys=True)` 启动 ensurepip/pip。离线重建入口及 pip `26.2.1` 检查通过，验证包含 `alter_sys=True` 的真实入口语义，不只依赖普通 `python -m pip` 的结果。

这些本机结果验证了官方完整包及离线 pip/venv 能力；正式主程序的新装与旧安装修复结果以下述 CI5 原始报告为依据。

### 当前 CI5 已完成的检查

以下结果来自修复后的 CI5 提交 `1ea507c`，运行记录为 [38704689185](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/38704689185)。整轮 CheckRun 已确认 `COMPLETED / SUCCESS`，完成时间为 `2026-10-15T08:41:22Z`；功能、测试、构建及工作流收尾均成功，以下结果有对应原始报告：

| 项目 | 当前证据 |
| --- | --- |
| TypeScript / 前端 | `tsc`、Vite 生产构建、49 项前端测试通过 |
| Rust | 严格格式检查、clippy、134 项 Rust 测试通过；4 项联网或发布产物测试在默认单元测试中显式忽略，相关联网覆盖另列 |
| 发布脚本 | 4 项 Node 纯 JavaScript 测试通过 |
| 官方源 | 15 类官方版本源实时查询通过 |
| 完整 Python 实际安装事务 | 通过：官方完整运行时下载、校验、安装初始化及 pip/venv 验收 |
| 完整 Python 独立冒烟 | `Smoke-Python.ps1` 的全新安装、真实旧嵌入式 ZIP 加 fresh 旧缓存的同版本原路径修复均通过；旧清单缓存被拒绝，安装清单迁移及完整开发功能检查通过 |
| 编译与 NSIS | 0.1.6 编译、NSIS 隔离安装及主程序一致性验证通过 |
| 命令模式 | 全盘扫描、15 类工具定义、110 个 CMD 脚本、目录持久化、生成脚本读取最新根目录、只读不扫描及无确认的预览保护通过 |
| 15 类工具真实安装 | 15/15 通过、0 失败；安装事务、清单、版本探测、切换预览、缓存和清理检查均通过 |

历史 CI4 提交 `47f4dfc`、运行 [38703167061](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/38703167061) 曾在完整 Python 实际安装时失败：`runpy` 令 `sys.argv[0]` 保持为 `pip`，触发 Windows pip 的自更新保护。该问题通过 `alter_sys=True` 修复；上表以 CI5 的实际通过结果为依据，CI4 整轮不计为成功。

### 完整 Python 新装与真实旧安装修复

原始证据为 `python-89fbe4b7280d4702a05eb750b789d91b/summary.json` 及对应 `reports/`，汇总为 `status: passed`、`complete: true`、`cleanup: passed`、`errors: []`。两个场景均使用正式 0.1.6 CLI：

| 检查 | 全新安装 | 真实旧嵌入式包修复 |
| --- | --- | --- |
| Python 版本 | `3.15.0` | 保持 `3.15.0`，`sameVersion: true` |
| 发行包 | 官方 `python-3.15.0-amd64.zip` | 从真实 `python-3.15.0-embed-amd64.zip` 迁移到完整 ZIP |
| 旧缓存与路径 | 新建隔离版本目录 | fresh 旧嵌入式清单被拒绝，`oldCatalogRejected: true`，安装路径保持不变 |
| pip | 模块、`pip.exe --version`、两种 `pip list` 均通过，版本 `26.2.1` | 同左，版本 `26.2.1` |
| 开发组件 | Tk `9.0`、IDLE 导入、`include/Python.h`、3 个导入库通过 | 同左 |
| venv | 独立 prefix、正确 base prefix、venv 内 pip 模块和入口、`pip list` 及小程序结果通过 | 同左 |
| 用户环境 | `unchanged` | `unchanged` |

Python 冒烟整体执行前后的用户环境指纹相同，临时目录清理通过。该修复场景实际下载并校验旧嵌入式 ZIP，再写入同版本的旧安装清单和 fresh 旧版本缓存；执行 repair 前没有先刷新清单，因此验证了生产修复流程自行拒绝旧缓存并替换原路径。

### 全工具安装、命令模式与环境恢复

原始证据为 `installability-9cd36e4ebbb143d78e883a201a303eeb/summary.json`：`complete: true`、`passedCount: 15`、`failedCount: 0`、`userEnvironment: unchanged`，每项 `cleanup: passed`。

| 工具 | CI5 实测版本 |
| --- | --- |
| Python | `3.15.0`（完整运行时） |
| Java | `27+35` |
| Go | `1.27.2` |
| Rust | `1.99.0` |
| Node.js | `26.12.0` |
| Git | `2.56.0.windows.2` |
| Android SDK Command-line Tools | `23.0` |
| Android NDK | `30.0.16248370` |
| Gradle | `9.8.1` |
| CMake | `4.4.4` |
| ADB / Platform Tools | `37.0.1` |
| Maven | `3.10.0` |
| .NET SDK | `10.0.402` |
| Ruby | `4.0.7-1` |
| PHP | `8.5.11` |

ADB 实际执行当前用户环境激活，`activationApplied`、`activationDefaultVerified`、`userEnvironmentRestored` 均为 `passed`，回读 PATH/Android 变量及默认版本后恢复原环境。其余 14 类工具仅验证切换计划，未实际应用；系统 PATH 遮蔽仍被明确阻止，安装成功不表示已经成为默认版本。

命令模式证据为 `smoke/cli-9503a681bf8744368bf9e7665a97b71c/summary.json`：记录了完成的全盘扫描、15 类定义和 110 个生成命令；根目录持久化、旧生成脚本读取新根目录、只读命令不扫描、预览保护及 JDK/修复/刷新脚本检查均通过。自动检查原始日志为 `ci-logs/verification.log`，联网结果为 `live-sources.log` 和 `live-install.log`。

### 安装包与签名验证

`ci-logs/installer-payload.json` 记录版本 `0.1.6`，NSIS 安装后的主程序与便携版 SHA-256 均为 `2fd8e082fc89e0192463734b8eb1f48db527da9a0098bccdcb04cd6339f34242`。

签名在本机使用原有长期密钥完成，未向云端上传私钥。独立签名验证运行 [38706313686](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/38706313686) 已完成，最终结论 `success`；该工作流从草稿下载六项资产，检查版本、下载 URL、签名侧文件和完整 SHA-256 清单，并使用应用内公钥执行真实 Rust 签名校验。

[PR #5](https://github.com/PuppetWen/EnvNexus-AI/pull/5) 已合并到 `main`（`b1f7554`）；该提交与 CI5 已验证的 `1ea507c` 仅有三份文档的差异。版本 [v0.1.6](https://github.com/PuppetWen/EnvNexus-AI/releases/tag/v0.1.6) 于 `2026-10-15T09:00:11Z` 正式发布。未携带 GitHub 凭据访问公开 `releases/latest/download/latest.json`，版本为 `0.1.6`，SHA-256 为 `005b69369466a250b1bf90a699af8f9c9cb6bc37f645525fa5b6233ad619daf4`，与已验证清单一致。安装包、便携包、两个签名侧文件、`latest.json` 和 `SHA256SUMS.txt` 六项资产从公开地址重新下载成功，逐文件哈希与本机已验证资产一致；下载后的两个包再次通过签名验证。

### 验证边界与公开交付状态

Python 修复后的独立 PATH 确认已由前端测试覆盖；本节的 CI 新装和修复通过不表示已在用户开发机执行真实 Python PATH 切换或完成本机 GUI 复测。尚未逐一安装全部 47 个历史 Python 完整运行时，也没有验证所有第三方包、原生扩展编译、IDE 集成或每个工具的真实多版本切换及卸载恢复。升级应用不会自动替换已有的嵌入式 Python，用户仍需在该版本卡片点击“修复”；修复会重建版本目录，自定义文件和第三方包需事先备份。

## 0.1.5 验证记录（2026-10-15 UTC）

本节记录 0.1.5 的新验证结果；后文保留 0.1.4 及更早版本的日期、数据和历史失败，不能把旧版 GUI 结果视为本版复测。

执行环境：GitHub Actions `windows-latest`、Windows x64 MSVC、Rust stable、Node.js 24、pnpm 11。本节以 CI7 提交 `79c50cc` 的完整测试报告为最终证据，运行记录为 [38689901318](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/38689901318)，最终结论 `success`：全部测试和打包步骤通过，包括命令模式全盘扫描、15 类工具真实安装、ADB 实际激活与用户环境恢复。此前 CI6 提交 `a8526f2` 的 15 类安装检查也已通过。

合并到 `main` 后，提交 `39e618f` 的[完整复测 38692450907](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/38692450907) 也以 `success` 完成。该次 CI 不再自动格式化或改写源码，严格格式检查、全部自动测试、联网源查询、构建与 NSIS 校验、全盘扫描和 110 个脚本、15/15 实际安装及 ADB 激活/恢复均再次通过。

### 自动检查与构建

| 项目 | 执行与结果 |
| --- | --- |
| TypeScript | `tsc --noEmit` 通过 |
| 前端单元测试 | Vitest 16 个测试文件、49/49 通过；包括安装完成后打开环境计划、单独确认写入以及仅保留安装的交互测试 |
| 前端生产构建 | Vite 构建通过，1794 modules；主 JavaScript chunk 仍有超过 500 kB 的体积警告 |
| Rust 格式与静态检查 | 格式检查及 `cargo clippy --all-targets -- -D warnings` 通过 |
| Rust 单元测试 | 124 通过、0 失败；4 项发布产物或联网测试在默认单元测试中显式忽略，联网覆盖另见下文 |
| 便携包准备脚本 | CI7 的 4 项 Node 纯 JavaScript 测试通过：NSIS 运行标记匹配、非 PE 拒绝、缺失/歧义标记拒绝、拒绝破坏已有 Authenticode 签名 |
| 15 官方源实时查询 | CI6、CI7 均通过；每一类均返回可用的 HTTPS Windows 发行包 |
| Python 实际安装事务 | CI6、CI7 均通过；官方嵌入式 ZIP 下载、SHA-256 校验、安全解压、暂存提交、安装清单和版本探测成功 |
| Release 与 NSIS | CI7 生成 `EnvNexus-AI_0.1.5_x64-portable.exe` 和 `EnvNexus-AI_0.1.5_x64-setup.exe`；隔离静默安装后，主程序 SHA-256 与便携包一致，安装后的 `tools --json` 返回 15 类工具 |

以上 CI 构建没有使用云端私钥。新安装包和便携包已在本机使用原有长期密钥签名；本机 Node 验证器通过旧版两个包和本版两个包的签名校验，并确认篡改内容会被拒绝。

独立 `verify-signed-release.yml` 已通过，提交 `7e89837`，运行记录为 [38693512678](https://github.com/PuppetWen/EnvNexus-AI/actions/runs/38693512678)，最终结论 `success`。该作业实际下载六项发行资产：两个包、两个签名侧文件、`latest.json` 和 `SHA256SUMS.txt`；确认 PE 版本、下载 URL、签名侧文件及严格五项 SHA-256 清单一致，并通过应用更新器的 Rust 测试 `built_release_assets_match_signed_metadata`，使用应用内公钥验证两个包的签名和元数据。

首轮使用只读令牌读取草稿发行版时返回 `release not found`，未进入签名校验；将该验证作业的仓库内容权限改为 `contents: write`、同时禁用 checkout 凭据持久化后重跑成功。私钥始终留在本机，没有上传 CI。

正式发布后，未携带 GitHub 凭据访问公开自动更新入口 `releases/latest/download/latest.json`，返回版本 `0.1.5`，内容 SHA-256 与已验证元数据相同。安装版、便携版、两个签名侧文件和校验清单的公开链接均重新下载成功，逐文件哈希一致，下载后的两个包再次通过签名校验。

### 15 类工具实际安装

CI7 的 `Smoke-Installability.ps1` 在全新隔离目录逐一选择当时最高稳定版本，使用正式 CLI 安装流程实际下载和安装，15/15 通过、0 失败；与 CI6 实测版本一致：

| 工具 | 实测版本 |
| --- | --- |
| Python | `3.15.0` |
| Java | `27+35` |
| Go | `1.27.2` |
| Rust | `1.99.0` |
| Node.js | `26.12.0` |
| Git | `2.56.0.windows.2` |
| Android SDK Command-line Tools | `23.0` |
| Android NDK | `30.0.16248370` |
| Gradle | `9.8.1` |
| CMake | `4.4.4` |
| ADB / Platform Tools | `37.0.1` |
| Maven | `3.10.0` |
| .NET SDK | `10.0.402` |
| Ruby | `4.0.7-1` |
| PHP | `8.5.11` |

每一项都确认安装事务 `committed`、受管目录与安装清单一致、版本可探测、切换计划包含环境差异、官方清单可从持久化缓存回读，以及本轮工具目录/下载缓存可安全清理。Rust 的文件符号链接只解除链接本身，目录链接和越界目录仍被拒绝；PHP 的官方重定向下载及校验通过。逐项失败会独立记录，整轮汇总必须全部成功才算通过。

CI7 除下文单独验证的 ADB 外，没有应用其余工具的切换计划；ADB 恢复后，整轮用户环境前后指纹一致。已有系统 PATH 遮蔽的工具在计划中明确要求管理员介入；安装和预览通过不表示已越过系统 PATH 或已成为默认版本。Python 测试对象是官方嵌入式 ZIP，不包含完整安装器、pip 或所有第三方扩展场景；Android SDK 项目仅覆盖 Command-line Tools，不代表已安装平台镜像、模拟器或 Android Studio。SDK、Gradle、Maven 的版本探测使用 runner 已提供的 Java。

最终原始证据保存在 CI7 报告 artifact：`installability-53c74578aee04007a357458d8d1810f8/summary.json` 及其 `reports/<tool>/` 中的 catalog、安装清单、安装结果和环境计划。汇总为 `complete: true`、`passedCount: 15`、`failedCount: 0`、`userEnvironment: unchanged`。此前 CI6 证据为 `installability-f28502d8649f42ada6b6bd05a70e4558/summary.json`。

### 命令模式与真实环境写入

CI7 的 `Smoke-Cli.ps1` 通过：15 类工具、110 个 CMD 脚本、JDK 别名、目录持久化、生成脚本读取后续保存的新根目录、显式全盘扫描、增量刷新和无 `--yes` 的预览保护。没有扫描快照时，只读命令不触发全盘扫描；预览环境修复不改变用户环境。最终证据为 `smoke/cli-5eae2507a7a84430bfaddd623d6a30d4/summary.json`；此前 CI6 同项也通过。

CI7 的 ADB 实际激活测试通过：在隔离 GitHub runner 中执行 `use --yes`，回读 HKCU `Path`、`ANDROID_HOME`、`ANDROID_SDK_ROOT` 与计划一致，刷新后确认默认 ADB 可执行文件等于刚安装的文件。执行前快照每个计划涉及值的原始未展开内容、类型和不存在状态，结束时在 `finally` 中精确恢复；结果如下：

| 断言 | CI7 结果 |
| --- | --- |
| 实际写入 `activationApplied` | `passed` |
| 默认版本与变量回读 `activationDefaultVerified` | `passed` |
| 原值、类型与不存在状态恢复 `userEnvironmentRestored` | `passed` |
| 整轮用户环境指纹 | `unchanged` |

本次 ADB 计划不需要提升权限。脚本仍拒绝需要提升权限的计划，不绕过系统 PATH；其余 14 类工具没有执行真实环境切换。

### 回归范围、缓存与速度边界

本版已通过的单元测试覆盖 GitHub 限流只请求一次并报告重置时间、官方发布页备用解析和摘要冲突拒绝、Node 所选发行包按需读取校验值、清单缓存回读；也覆盖并发 Range 下载的范围与实体校验、服务器不支持时回退单连接、续传、取消及临时分块清理。进程测试确认并发排空 stdout/stderr、防止解压输出塞满管道、超时有界，以及缺少原生 DLL 时返回错误而不弹出加载器窗口；ADB 两个伴随 DLL 的完整性检查通过。

源码验证脚本和 CI 使用 `Use-ProjectCache.ps1`，把项目 Cargo/Rustup、编译产物、pnpm/npm 和编译临时缓存放在项目目录内；本开发机为 E 盘，CI 为 runner 的 D 盘工作区。真实安装测试的 Gradle、.NET、Android、NuGet、Go 及 TEMP/TMP 缓存另隔离于本轮目录，完成后恢复进程环境。

2026-10-15 UTC 的本机代理样本只请求同一 Java 官方包的前 4 MiB：单连接约 6.56 秒；四路请求约 9.85 秒且只有三路成功。因此该样本没有证明下载提速，不能承诺“4 倍速度”。清单缓存可避免重复联网，并发下载的收益仍取决于代理、上游限速和链路质量；CI 安装耗时也不能代表用户本机带宽。

本机 Windows 应用程序控制阻止了当前 Rust 工具链、部分 Node 原生模块，以及 CI7 新便携包的普通 `tools --json` 启动；未绕过该策略。新便携包仅在本机静态确认 PE FileVersion 为 `0.1.5`，SHA-256 为 `e1a9214944a7cb357d55888ae5ffe4e9cfe31e03bda8f81dc07ce63f40a843c5`。本版原生构建、集成测试和安装包安装验证由 CI 完成；本机没有运行新的真实 HKCU 写入，也没有完成本版新 EXE、GUI 或托盘交互复测。尚未验证每种工具的编译项目、包管理器生态、多版本实际切换、卸载恢复，以及真实 AI 账号在线推理；这些边界不因 15 类安装通过而消失。

## 0.1.4 历史测试报告

测试日期：2026-07-31
系统：Windows 11 Home China `10.0.26200` x64  
构建工具链：Rust `1.97.1`、Tauri `2.11.5`、Node.js `25.2.1`、pnpm `11.9.0`

### 自动测试

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

### 2026-07-31 全机扫描与性能复测

- 完整扫描遍历本机 C、D、E、F 四个固定磁盘，索引覆盖 15 类工具；测试快照包含 161 个已验证安装版本。
- 同一全机索引首次执行增量探测用时 33.188 秒；生成 `tool-version-probes.json` 后，两次缓存刷新分别用时 2.478 秒和 2.379 秒。
- 首次与缓存刷新均返回 15 类工具、161 个安装版本，数量一致；缓存刷新仍实时重新验证当前 PATH 默认工具。
- 指纹包含可执行文件大小、修改时间，以及 Android NDK 的 `source.properties` 元数据；文件发生变化后缓存测试确认会拒绝复用并重新执行版本命令。
- 空闲界面没有定时扫描或轮询。Windows WebView2 在窗口失焦、隐藏或进入托盘时切换到低内存目标，恢复窗口后切回正常目标；后台 Rust 下载与更新任务不暂停。
- 在相同的全新隔离数据目录、窗口最小化并失焦 30 秒后，对整个 7 进程树连续采样 8 秒：0.1.3 平均工作集 436.1 MiB，0.1.4 平均工作集 101.6 MiB、峰值 102.8 MiB，物理驻留工作集下降约 76.7%。
- 同一采样窗口内，两版均未观察到可计量的空闲 CPU 增长。0.1.4 平均私有提交量为 401.2 MiB，与 0.1.3 的 400.1 MiB 基本一致，因此不将此次优化表述为虚拟提交量下降。
- 最终便携包的 CLI 冒烟通过：全机扫描、增量刷新、15 类工具、110 个脚本、JDK 别名、根目录动态读取和无确认令牌的预览保护均成功。托盘冒烟通过关闭到托盘、页面恢复、启动隐藏、开机启动、语言切换和正常退出。

### 2026-07-24 EnvNexus AI 改名与更新器复测

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

### 实时官方源

同一联网测试实际请求并解析：

```text
python, java, go, rust, node, git, maven, dotnet, ruby, php,
android-sdk, android-ndk, gradle, cmake, adb
```

测试要求每个 catalog 非空，且至少包含一个 HTTPS Windows 下载 URL。2026-07-23 的完整聚合复测运行两次，均在 `https://go.dev/dl/?mode=json&include=all` 连接超时后失败（约 81 秒与 115 秒）；这项结果记录为失败。新增四源独立复测通过，用时 17.90 秒：Maven 1、.NET SDK 18、Ruby 24、PHP 6。

### 实际下载、校验和安装

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

### Release 构建与打包

Tauri Release 和 NSIS 均已成功：

| 文件 | 大小 | SHA-256 |
| --- | ---: | --- |
| `EnvNexus-AI_0.1.4_x64-portable.exe` | 10,848,768 bytes（10.35 MiB） | `9884bbcd0c9876799071e1de505eb484d9c2d92b2ad21e2e8b166c3c05da8f32` |
| `EnvNexus-AI_0.1.4_x64-setup.exe` | 5,181,314 bytes（4.94 MiB） | `3a555abe72cfc46617824048b28a57fd0cad8d7a7fb8ece1abfe26f9cf3a56e7` |

校验文件：`release/SHA256SUMS.txt`。

对最终复制到 `release` 的 portable 文件再次执行单实例与命令模式冒烟：第二个 GUI 进程退出码为 0，原进程保持运行并恢复隐藏窗口；`tools --json` 返回 15 个工具，隔离数据目录中没有生成扫描快照。两份新 Release 文件的重新计算哈希均与 `SHA256SUMS.txt` 匹配。

当前开发机链接时出现 `LNK4099`：项目内 xwin SDK 静态运行库没有调试 PDB。链接仍成功，Release 可启动；该警告已记录，不解释为测试失败。前端构建还提示主 JavaScript chunk 为 531.77 kB，超过默认 500 kB 警告阈值；gzip 后为 137.11 kB，构建成功，后续可通过页面级拆包继续优化。

### 运行冒烟

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

### 未执行的高影响测试

- 没有修改现有 HKCU/HKLM/PATH；
- 没有申请管理员权限；
- 没有卸载或迁移现有工具；
- 没有在开发机上逐一实际安装 Java、Go、Rust、Node、Git、Android、Gradle、CMake；
- 没有使用真实厂商 API Key 请求 AI 模型列表或执行推理；不会伪造这类联网结果；
- 没有安装生成的 NSIS 包，以免向系统安装目录、卸载项写入；已验证 NSIS 生成与哈希。

这些未执行项不会被描述为已验证，详见 `KNOWN_LIMITATIONS.md`。
