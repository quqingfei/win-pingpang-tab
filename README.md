# FastTabClick

一款 Windows 平台下的增强型窗口切换工具，通过自定义快捷键快速在不同窗口间切换。

## 功能特性

- **快捷键触发**：支持自定义修饰键（Alt、Ctrl、Shift、Win 等）组合 + 鼠标键（右键、中键、X1、X2）
- **智能窗口切换**：自动构建当前光标下所有窗口的堆栈，一键切换到下一个窗口
- **进程黑名单**：可配置忽略特定进程，触发时原样放行鼠标事件
- **系统托盘**：后台运行，托盘菜单提供快捷操作
- **开机自启**：支持开机自动启动
- **日志记录**：分级日志记录，方便问题排查
- **单实例运行**：防止重复启动

## 支持的快捷键

- **修饰键**：`lalt` / `ralt` / `lctrl` / `rctrl` / `lshift` / `rshift` / `lwin` / `rwin`（任意组合，至少1个）
- **触发鼠标键**：`right` / `middle` / `x1` / `x2`（不包含 left）

## 配置文件

首次运行后会自动在 `%APPDATA%\fast-tab-click\` 目录下生成 `config.toml` 配置文件：

```toml
# 修饰键配置
modifiers = ["lalt"]

# 触发鼠标键
button = "right"

# 进程黑名单（命中时放行鼠标事件，不切换窗口）
blacklist = ["explorer.exe"]

# 是否以管理员权限启动
elevated = false

# 日志级别：off | error | info | debug
log_level = "info"

# 日志文件大小上限（MB）
max_log_size = 10
```

## 构建要求

- Rust 1.70+
- Windows SDK
- Visual Studio Build Tools

## 编译

```bash
cargo build --release
```

编译后的可执行文件位于 `target/release/fast-tab-click.exe`。

## 使用说明

1. 首次运行后会自动创建配置文件
2. 根据需要修改配置文件中的快捷键和黑名单
3. 重启应用使配置生效
4. 使用配置的快捷键（按住修饰键 + 点击鼠标键）即可切换窗口

### 托盘菜单

- **以管理员身份重启**：以管理员权限重新启动应用
- **打开配置文件夹**：打开配置文件所在目录
- **开机自启**：勾选启用/禁用开机自启
- **退出**：关闭应用

## 已知限制

- 不支持 left（左键）作为触发键
- 部分特殊窗口（如 UWP cloaked 窗口）可能被忽略

## 技术栈

- Rust
- Windows API（Win32）
- Tauri（可选）

## 许可证

MIT License