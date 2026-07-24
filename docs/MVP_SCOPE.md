# EnvNexus AI MVP 范围与验收

## MVP 必须完成

- 可运行的 Windows Tauri 桌面程序。
- 五套可即时切换并持久化的 UI 主题。
- 统一工具插件注册表。
- Python、Java、Go、Rust、Node.js、Git、Maven、.NET SDK、Ruby、PHP、Android SDK/NDK、Gradle、CMake、ADB 的真实检测。
- 展示当前默认版本、全部发现版本、位置和环境健康状态。
- 从官方源查询可安装版本，显示来源与查询时间。
- 至少对官方 ZIP/TAR 发行物完成：
  - 自选目录；
  - 断点续传；
  - SHA-256；
  - 安全解压；
  - 安装清单；
  - 验证；
  - 失败回滚。
- 环境变量/PATH 变更计划：
  - 差异预览；
  - 重复、失效、遮蔽与冲突检测；
  - 用户确认；
  - HKCU 备份；
  - 应用、广播与恢复；
  - 默认拒绝 HKLM。
- App 管理版本的切换、修复和卸载。
- Android 子工具集中到同一用户选择根目录。
- 操作日志、环境诊断、事务 journal 和普通失败回滚。
- 与 GUI 共用领域服务的工具级 CMD 脚本，覆盖列举、扫描、目录、官方版本、安装、切换、修复和卸载。

## 验收证据

- `cargo test` 结果；
- 前端测试结果；
- `cargo clippy` 结果；
- TypeScript/Vite 生产构建结果；
- Tauri Windows 单一可执行文件、工具 CMD 脚本和 NSIS 安装包路径、大小及 SHA-256；
- 一份基于本机真实扫描的手工冒烟测试记录；
- 构建说明、使用文档与限制清单。

## 不冒充完成的事项

以下内容若未被实际验证，必须在最终报告中明确标记：

- 未实际下载的远程发行物；
- 未实际执行的安装器；
- 未提升权限验证的系统级写入；
- 因网络或供应商 API 变化而未通过的源；
- 只通过 fixture、没有在线验证的解析器。

实际结果见 `TEST_REPORT.md`。崩溃后自动恢复 journal、系统级写入、完整升级编排等未纳入 MVP 的项目见 `KNOWN_LIMITATIONS.md`。
