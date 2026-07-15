# TakeFive Agent 指南

## 项目概览

TakeFive 是一个 Windows / macOS 本地优先的桌面提醒应用。仓库采用 Tauri 2、Rust、React/TypeScript 和 SQLite：Rust 负责可靠调度、领域状态、持久化和平台事件；前端负责窗口内交互与展示。

## 目录边界

- `crates/domain`：纯领域模型、时间规则、Occurrence 状态机和可注入时钟。不依赖 Tauri、SQLite 或具体操作系统。
- `crates/application`：用例编排、事务边界和应用服务，连接领域层、持久化层与调度层。
- `crates/persistence-sqlite`：SQLite 连接、迁移、Repository 和数据库错误。
- `crates/scheduler`：候选事件计算、调度策略和恢复/重建逻辑。
- `apps/desktop/src-tauri`：Tauri 进程、IPC 命令、窗口/托盘、通知和平台适配。
- `apps/desktop/src`：React/TypeScript 界面。不得把权威计时、调度决策或数据库访问放在前端。
- `docs`：产品、架构、阶段计划和验收文档；行为变化涉及规则或 IPC 时应同步更新。
- `artifacts`、`output`、`target`：构建、运行和测试产物，除非任务明确要求，不要手工修改或提交。

## 开发约定

- 使用 Rust 2021 edition 和 workspace 依赖；提交前保持 `cargo fmt` 结果干净。
- 领域逻辑优先使用纯函数、显式输入和可注入时钟，避免读取系统时间或依赖真实等待。
- SQLite 是事件状态的唯一事实来源；内存队列必须可从数据库重建，不能作为持久状态。
- 同一计划事件最多投递一次。涉及状态变更时保留现有 revision/原子认领和状态机约束，优先通过现有 Repository/应用服务修改，不绕过它们直接写库。
- 用户可见时间按本地时区展示，内部事件时间使用 UTC；规则计算必须明确处理时区、DST 跳时和回拨。
- 平台能力不可用时返回明确的 `Unavailable`/`Denied`/`Unknown` 语义，不伪造成功状态。
- IPC DTO 和命令保持向后兼容；修改命令名或字段时同时更新 Rust 调用方、前端调用方和相关测试。
- 前端沿用现有 React、TanStack Query、CSS 和 `lucide-react` 约定；业务事实通过 IPC 获取，不在组件中复制领域规则。
- 注释只解释非显然的约束或边界情况；不要提交生成目录、日志、截图或本地数据库文件。

## 常用命令

在仓库根目录执行：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

在 `apps/desktop` 执行：

```powershell
npm install
npm run build
npm run tauri dev
npm run tauri build -- --no-bundle --ci
```

涉及界面流程时，优先补充或运行现有 Playwright 冒烟检查，并检查窄窗口和桌面窗口布局。涉及调度、DST、休眠/唤醒、锁屏/解锁或时间变化时，必须增加相应的 Rust 单元/集成测试，而不是只验证 UI。

## 变更流程

1. 先阅读相关 crate、调用方和对应设计文档，确认改动所属层级。
2. 保持变更最小化，沿用已有类型、错误和事件命名；不要顺手重构无关代码。
3. 先运行受影响模块的快速测试，再运行完整 workspace 校验；前端变更至少运行 `npm run build`。
4. 在交付说明中列出修改文件、验证命令和任何未运行的检查。若当前目录不是 Git 工作树，不要假设可以执行提交或查看提交历史。

## 重点风险

- 不要用前端 `setTimeout` 作为提醒是否到期的依据。
- 不要在休眠后补发一串过期通知；恢复时应按调度策略对账并合并/跳过。
- 不要把系统通知中心当作可靠队列；事件状态必须先在数据库中原子变更。
- 不要为了方便绕过 revision 冲突、Occurrence 状态机或迁移版本。
