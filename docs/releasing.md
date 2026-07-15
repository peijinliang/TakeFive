# TakeFive 发布与自动更新

TakeFive 使用 GitHub Releases 托管 Windows 安装包和 Tauri 更新清单，不需要业务服务器。客户端从以下固定地址检查最新正式版本：

```text
https://github.com/peijinliang/TakeFive/releases/latest/download/latest.json
```

## 更新安全

- Tauri 更新公钥保存在 `apps/desktop/src-tauri/tauri.conf.json`。
- 更新私钥只保存在维护者的安全备份和 GitHub Actions Secret `TAURI_SIGNING_PRIVATE_KEY` 中。
- 私钥不得提交到 Git、Issue、Release 或构建产物。
- 丢失私钥后，已安装客户端无法验证新密钥签署的更新，只能让用户手动重新安装，因此必须保留离线备份。
- Tauri 更新签名只负责验证更新来源，不等同于 Windows 代码签名。正式公开分发前仍建议为 NSIS 安装包配置 Windows 代码签名证书。

## 发布步骤

1. 在以下三个位置设置相同的 SemVer 版本号，且必须高于上一个正式版本：
   - `apps/desktop/src-tauri/tauri.conf.json`
   - `apps/desktop/src-tauri/Cargo.toml`
   - `apps/desktop/package.json`
2. 更新变更说明并完成完整质量检查。
3. 提交所有发布内容，然后创建并推送同版本标签，例如：

   ```powershell
   git tag v0.1.0
   git push origin v0.1.0
   ```

4. `.github/workflows/release.yml` 会在 Windows Runner 中构建 NSIS 安装包、签名更新包并生成 `latest.json`。
5. 工作流创建的是 Draft Release。先下载并完成 Windows 真机安装、升级、托盘退出和 SQLite 数据保留检查。
6. 验收通过后在 GitHub Releases 页面发布草稿。只有发布后的正式 Release 才会被客户端的 `releases/latest` 地址发现。

## 首次与日常验证

- 首次发布可以使用 `v0.1.0`；它建立下载入口，但不会向同为 `0.1.0` 的客户端提示更新。
- 验证自动更新时，先安装较低版本，再发布更高版本，例如从 `0.1.0` 升级到 `0.1.1`。
- 客户端启动 15 秒后后台检查一次；成功检查后 24 小时内不重复自动请求。设置页始终可以手动检查。
- 客户端不会静默安装。用户确认后才下载、安装并重启。
- Windows 安装器使用微软 WebView2 在线 bootstrapper 以保持包体轻量；仅在系统缺少 WebView2 时需要联网补齐运行时，应用安装完成后仍可离线运行。
- Release 产物上传和 `latest.json` 生成必须由同一个工作流完成，不要手工替换其中一个文件。

## 私钥维护

当前维护机器的私钥默认位于：

```text
%USERPROFILE%\.tauri\takefive-updater.key
```

将私钥重新写入 GitHub Secret 时，使用标准输入，避免在终端历史中出现私钥内容：

```powershell
Get-Content "$env:USERPROFILE\.tauri\takefive-updater.key" -Raw |
  gh secret set TAURI_SIGNING_PRIVATE_KEY --repo peijinliang/TakeFive
```

轮换密钥需要先通过旧密钥签署一个同时信任新公钥的过渡版本。不要直接替换客户端公钥后发布，否则旧客户端无法升级。
