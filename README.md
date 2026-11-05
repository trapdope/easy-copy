# EasyCopy

EasyCopy 是一款 macOS 剪贴板历史管理工具。它常驻菜单栏，自动记录文本和图片剪贴板内容，并通过全局快捷键在鼠标附近呼出一个轻量浮窗，方便快速搜索、选择和粘贴历史内容。

## 官网与下载

- 官网：<https://trapdope.github.io/easy-copy/>
- macOS Apple Silicon 下载包：[`docs/downloads/EasyCopy-0.1.0-macOS-arm64.dmg`](docs/downloads/EasyCopy-0.1.0-macOS-arm64.dmg)

## 功能特性

- 菜单栏常驻运行，不占用 Dock 图标
- 自动记录最近 100 条剪贴板历史，锁定的内容会长期保留
- 支持纯文本和图片记录
- 支持显示复制来源应用
- 支持搜索剪贴板历史
- 支持锁定并长期保留记录、添加可搜索的备注，以及删除单条历史记录
- 支持全局快捷键唤醒，默认 `Cmd+Shift+V`
- 支持自定义唤醒快捷键
- 支持开机启动
- 支持点击或回车后自动粘贴到之前聚焦的输入框
- 支持毛玻璃无边框浮窗
- 支持点击窗口外自动隐藏

## 权限说明

EasyCopy 的自动粘贴能力需要 macOS 辅助功能权限。

首次使用后，请打开：

```text
系统设置 > 隐私与安全性 > 辅助功能
```

然后为 `EasyCopy` 开启权限。

如果未开启辅助功能权限，EasyCopy 仍然可以记录和复制剪贴板内容，但无法稳定地把选中内容自动粘贴到目标应用。

## 安装使用

### 本机使用

构建 release 版本：

```bash
pnpm tauri build
```

构建完成后，将下面的 app 拖入 `/Applications`：

```text
src-tauri/target/release/bundle/macos/EasyCopy.app
```

第一次打开时，如果 macOS 拦截，请右键点击 `EasyCopy.app`，选择“打开”。

### 开机启动

打开 EasyCopy 浮窗右上角的设置按钮，开启“开机启动”。

建议先将 `EasyCopy.app` 放入 `/Applications` 后再开启开机启动，这样启动路径更稳定。

## 快捷键

默认快捷键：

```text
Cmd+Shift+V
```

可以在 EasyCopy 的设置面板中点击快捷键输入框并录制新的快捷键。

当浮窗已经显示时，再次按快捷键不会重复呼出或重置当前状态。

## 开发环境

依赖：

- macOS
- Rust
- Node.js
- pnpm

安装前端依赖：

```bash
pnpm install
```

启动前端开发服务：

```bash
pnpm dev
```

启动 Tauri 开发模式：

```bash
pnpm tauri dev
```

构建前端：

```bash
pnpm build
```

构建 macOS 应用：

```bash
pnpm tauri build
```

## 技术栈

- Tauri v2
- Rust
- Vue 3
- TypeScript
- Tailwind CSS
- pnpm

## 数据存储

EasyCopy 会将历史记录和设置保存在应用数据目录中：

- `history.json`：剪贴板历史
- `settings.json`：开机启动和快捷键设置

剪贴板数据仅保存在本机，不会上传到网络服务。

## 注意事项

- 当前主要面向 macOS 使用
- 自动粘贴依赖辅助功能权限
- 图片历史会占用更多本地存储空间
- 未签名构建在首次打开时可能触发 macOS 安全提示
