## 变更内容

<!-- 简要说明改了什么。 -->

## 解决的问题

<!-- 关联 Issue，并说明用户可见影响。 -->

## 验证

<!-- 列出实际运行的命令和手工场景；未运行的检查请说明原因。 -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `npm run build`（涉及前端时）

## 提交前检查

- [ ] 变更保持在当前问题范围内，没有无关重构或生成产物。
- [ ] 新增或修改的调度、DST、生命周期行为有 Rust 自动化测试。
- [ ] IPC 或用户可见规则变化已同步更新调用方和文档。
- [ ] 没有绕过 revision 冲突、Occurrence 状态机或 SQLite 原子认领。
- [ ] 没有提交日志、本地数据库、密钥、用户数据或绝对本机路径。
