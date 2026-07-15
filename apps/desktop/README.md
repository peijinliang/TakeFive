# TakeFive Desktop

这里是“摸个鱼 · TakeFive”的桌面应用入口，包含 React/TypeScript 界面和 Tauri 2 主进程。项目总览、功能状态与架构说明请从[仓库根 README](../../README.md)开始阅读。

## 本地运行

```powershell
npm ci
npm run tauri dev
```

只运行 `npm run dev` 会启动普通浏览器页面，但由于提醒数据和设置依赖 Tauri IPC，不能代表完整桌面应用行为。

## 常用命令

```powershell
npm run build
npm run tauri build -- --no-bundle --ci
```

Rust workspace 的格式、静态检查与测试命令需要在仓库根目录执行，详见[贡献指南](../../CONTRIBUTING.md)。
