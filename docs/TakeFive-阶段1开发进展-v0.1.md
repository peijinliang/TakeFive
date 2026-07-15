# 摸个鱼 · TakeFive 阶段 1 开发进展

**版本：** V0.1  
**日期：** 2026-07-14  
**状态：** 阶段 1 核心链路已建立，完整产品目标尚未完成

---

## 1. 本轮结果

本轮按三个互不重叠的并行任务开发，并由主任务完成统一调度集成：

| 并行任务 | 产物 | 独立测试 |
| --- | --- | ---: |
| 领域规则与状态机 | Reminder、Occurrence、固定/一次性/间隔规则 | 19 |
| SQLite 持久化 | migration、Repository、事务认领、软删除 | 9 |
| Windows 系统事件 | 休眠、锁屏、时间、时区、显示器变化 | 7 个平台测试包含在桌面测试中 |
| 主任务集成 | Scheduler、策略引擎、SQLite Adapter、桌面 IPC | 9 |

当前全仓 Rust 测试共 45 项，全部通过。

---

## 2. 已落地模块

```text
crates/domain
  时间、提醒定义、Occurrence 状态机、规则计算

crates/persistence-sqlite
  SQLite v2、migration、Repository、并发认领

crates/scheduler
  候选读取、策略判断、原子认领、投递协调

crates/application
  Scheduler 与 SQLite 的适配及端到端测试

apps/desktop
  Tauri IPC、Windows 平台事件、提醒增删查、React 界面
```

依赖方向保持为：

```text
desktop -> persistence/domain
application -> scheduler + persistence
scheduler -> domain + ports
persistence -> SQLite
domain -> 无基础设施依赖
```

---

## 3. 数据库能力

当前 schema 版本为 `2`，包含：

- `reminders`
- `schedule_rules`
- `reminder_policies`
- `occurrences`
- `delivery_attempts`
- `pause_sessions`
- `settings`
- `schema_meta`

已验证：

- WAL、外键、5 秒 busy timeout。
- `reminder_id + occurrence_key` 唯一约束。
- 两个并发认领只有一个成功。
- 配置事务失败时完整回滚。
- 提醒软删除后历史事件保留。
- 策略延期时间、原因、投递成功和投递失败可持久化。
- 延期事件可在模拟进程重启后重新进入投递链路。

Windows 发布版已真实创建：

```text
%LOCALAPPDATA%\com.takefive.desktop\takefive.db
```

---

## 4. 领域不变量

当前代码已自动验证：

- 终态 Occurrence 不可回退。
- 未展示事件不能直接完成。
- 延后只更新本次事件，不改变原计划时间和 occurrence key。
- 固定锚点间隔使用 anchor/index 计算，不随实际展示时间漂移。
- 会话间隔可在暂停、锁屏和无效时间段冻结剩余时长。
- 跨午夜生效时间段按 start-inclusive/end-exclusive 处理。
- DST 跳时顺延，回拨重复时刻只取第一次。

---

## 5. 调度链路

当前调度器已经实现：

```text
CandidateSource
  -> OccurrenceStore.claim
  -> PolicyEngine.evaluate
  -> record_decision
  -> DeliveryPort.deliver
  -> mark_presented / mark_delivery_failed
```

策略顺序覆盖：

1. 提醒启用状态。
2. 生效时间段。
3. 单条暂停。
4. 全局暂停。
5. 勿扰。
6. 锁屏/休眠。
7. 唤醒冷却。
8. 全屏状态。

重要提醒只有“提醒为重要”与“全局允许绕过”同时成立时，才能绕过全局暂停或勿扰。

### 真实 SQLite 端到端证据

- 同一候选连续执行两次 reconcile，只投递一次，数据库最终状态为 `presented`。
- 全局暂停产生 `suppressed + deferred_until + reason`。
- 模拟重启后读取 due deferred occurrence，不二次认领并继续投递。

---

## 6. Windows 平台事件

已接入：

- `sleep` / `wake`
- `lock` / `unlock`
- `time_changed`
- `timezone_changed`
- `display_changed`

原阶段 0 使用 `HWND_MESSAGE`，但该窗口收不到所有系统广播。本轮改为不可见顶层原生窗口，同时阻止显示与任务栏出现。

时区变化不直接信任任意 `WM_SETTINGCHANGE`，而是比较 `GetDynamicTimeZoneInformation` 前后快照，仅真实变化时上报。

仍需人工演练真实休眠、锁屏、改时间、切时区和显示器拔插。

---

## 7. 桌面闭环

发布版已经能够：

- 启动时创建应用数据目录和 SQLite 数据库。
- 查询数据库健康状态与 schema 版本。
- 创建本地提醒。
- 列出未删除提醒。
- 软删除提醒并保留历史。
- 关闭主窗口时销毁 WebView，保留 Rust/托盘核心。
- 再次启动时唤起单一实例并重建窗口。

界面自动化验证未完成：Windows 当前处于锁屏状态，按照桌面自动化安全要求已停止输入。发布版启动与数据库创建已经验证，创建提醒的界面操作需桌面解锁后复测。

---

## 8. 工程门禁

| 检查 | 结果 |
| --- | --- |
| Rust 格式检查 | 通过 |
| Clippy `-D warnings` | 通过 |
| 全 workspace 测试 | 45/45 通过 |
| TypeScript 类型检查 | 通过 |
| Vite 生产构建 | 通过 |
| Tauri Windows Release 构建 | 通过 |
| 发布版启动与 SQLite 创建 | 通过 |
| 发布版提醒 UI 操作 | 待桌面解锁复测 |

---

## 9. 当前需求覆盖

| 需求域 | 状态 | 说明 |
| --- | --- | --- |
| 固定时间 | 核心完成 | 规则与 DST 测试完成，尚未后台自动循环 |
| 间隔循环 | 核心完成 | 固定锚点和会话计时模型完成 |
| 一次性提醒 | 核心完成 | 规则计算完成 |
| 连续使用电脑 | 部分完成 | 空闲时长探针完成，完整 activity cycle 未接调度 |
| 事件状态机 | 核心完成 | 完成、跳过、延后、未处理等转换已建模 |
| 本地数据库 | 核心完成 | schema v2 与恢复查询基础已完成 |
| 暂停与勿扰 | 部分完成 | 策略完成，编辑与持久化查询未完成 |
| 系统生命周期 | Windows 接入 | 真实设备演练与 macOS 未完成 |
| 系统通知 | 技术接入 | 尚未由后台 scheduler 自动投递 |
| 四个主页面 | 未完成 | 当前仍为架构验证与最小提醒界面 |
| 统计、主题、备份 | 未完成 | 后续阶段 |
| macOS | 未验证 | 需要 macOS 真机与签名环境 |

完整需求仍保持原范围，没有把未完成项从目标中删除。

---

## 10. 下一批并行任务

建议继续拆分：

| 任务 | 目标 |
| --- | --- |
| A. SQLite CandidateSource | 从启用提醒规则与延期事件生成 due candidates |
| B. 后台 Scheduler Loop | 单一计时器、启动恢复、平台事件触发 reconcile |
| C. 暂停/勿扰上下文 | pause_sessions、固定勿扰、重要提醒绕过配置 |
| D. 提醒编辑器 | 名称、固定时间、工作日、启用状态和下一次时间 |

下一批合并后应完成一个真正可用的最小闭环：创建工作日固定提醒，关闭主窗口后仍由后台按时发送系统通知，重启和时间回拨不重复。

