# 参与贡献

感谢你愿意改进“摸个鱼 · TakeFive”。项目当前处于 Windows MVP 预览阶段，优先处理提醒可靠性、低打扰体验、数据安全和可复现的问题。

## 开始之前

- Bug 和功能建议请先搜索现有 Issue，避免重复。
- 较大的功能或架构调整请先开 Issue 说明用户问题、范围和取舍，再开始实现。
- 安全漏洞不要提交到公开 Issue，请按[安全策略](SECURITY.md)报告。
- 项目当前不接受把账号、云同步、广告、遥测默认上传或医疗建议引入核心产品的变更。

## 开发环境

- Rust stable
- Node.js `20.19+` 或 `22.12+`
- Tauri 2 对应的系统依赖
- Windows 10/11 是当前主要验证平台

```powershell
cd apps/desktop
npm ci
npm run tauri dev
```

## 架构边界

- `crates/domain` 保持纯领域逻辑，不依赖 Tauri、SQLite 或操作系统。
- 权威计时、调度决策和数据库访问放在 Rust 端，不放入 React 组件。
- SQLite 是事件状态的唯一事实来源；内存队列必须能从数据库重建。
- 涉及事件状态时，使用现有 Repository、revision 冲突保护和 Occurrence 状态机。
- 用户可见时间按本地时区展示，内部事件时间使用 UTC，并明确处理 DST。
- 平台能力不可用时返回明确状态，不伪造成功。

更完整的约束见仓库根目录的 `AGENTS.md` 和[当前技术架构](docs/TakeFive-技术架构设计-v1.0.md)。

## 提交前检查

在仓库根目录执行：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

前端变更还需执行：

```powershell
cd apps/desktop
npm ci
npm run build
```

界面流程变更应补充或运行 Playwright 冒烟检查，并检查窄窗口与桌面窗口布局。调度、DST、休眠/唤醒、锁屏/解锁或系统时间变化必须增加 Rust 自动化测试。

## Pull Request 要求

- 只包含解决当前问题所需的修改，避免顺手重构无关模块。
- 清楚描述用户可见变化、实现边界和验证方法。
- 行为涉及规则、IPC 或发布状态时同步更新对应文档。
- 不提交 `target`、`node_modules`、`dist`、`output`、`artifacts`、日志、本地数据库或密钥。
- 不降低现有去重、原子认领、状态机或 revision 冲突约束。

提交 PR 即表示你同意以本项目的 [MIT License](LICENSE) 发布你的贡献。
