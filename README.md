# WinPingPangTab

一款 Windows 后台窗口快速切换工具：**左alt + 鼠标右键，直接把鼠标下方被盖住的窗口切到最前**——不需要先最小化/移开当前窗口，也不需要精准点中目标窗口露出的那一小条。

## 为什么需要它

日常开发时经常在 IDE、IM（微信/钉钉/飞书）、浏览器等多个窗口间来回切换。传统方式要么 Alt+Tab 遍历查找，要么先移开手用鼠标点任务栏——窗口一多，目标难找、节奏被打断。

FastTabClick 的思路：**窗口在屏幕上的位置你已经知道，直接对着它按一下就能切过来**。按住左 Alt + 右键点在"微信所在的区域"，微信立刻置前；再按一次，切回原来的窗口。

## 使用方式

1. 运行 `win-pingpang-tab.exe`（程序常驻后台，任务栏托盘出现图标）；
2. 鼠标移到被盖住的目标窗口区域上（哪怕被前台窗口完全盖住也行，只要你知道它在那）；
3. **按住左 Alt + 点右键**，松开即切换：目标窗口立刻激活置前；
4. 在同一位置再次触发，两窗口来回乒乓切换。

托盘右键菜单：

| 菜单项 | 功能 |
|---|---|
| 配置文件 | 打开 `config.toml` 自定义触发键、黑名单、日志等 |
| 开机自启动 | 勾选后开机自动运行 |
| 退出 FastTabClick | 结束程序（卸载钩子，清理托盘） |

## 触发键自定义（config.toml）

配置文件位于 `%APPDATA%\win-pingpang-tab\config.toml`，修改后重启程序生效：

```toml
[trigger]
# 修饰键：lalt ralt lctrl rctrl lshift rshift lwin rwin（至少 1 个，可多选）
modifiers = ["lalt"]
# 鼠标键：right（右键）middle（中键）x1（侧键1）x2（侧键2）
button = "right"

[blacklist]
# 悬停窗口属于这些进程时触发键原样放行（如远程桌面 mstsc.exe 自带 Alt+右键）
processes = ["mstsc.exe"]

[log]
level = "info"        # off / error / info / debug
max_size_mb = 10      # 日志超上限自动清空重写
```

字段缺失或非法时自动回落默认值，不会导致启动失败。

## 特性

- **零打扰**：不按键时对系统零影响，纯右键/纯 Alt 完全放行原生行为；
- **轻量**：常驻内存约 12 MB，空闲 0% CPU，单文件 exe（约 650 KB）无依赖；
- **多显示器/混合 DPI**：Per-Monitor Aware v2，任意屏、任意缩放下命中准确；
- **窗口跨屏拖动**：无状态设计，窗口拖到哪里就在哪里被找到；
- **单实例**：重复启动自动退出，不会装多个钩子；
- **日志**：`%APPDATA%\win-pingpang-tab\logs\app.log`，每次切换均有记录，10 MB 超限清空。

## 下载与构建

- **直接使用**：从 [Releases](https://gitee.com/quqingfei/win-pingpang-tab/releases) 下载 exe 即可；
- **源码构建**：安装 [Rust](https://rustup.rs/) 后执行：

```bash
cargo build --release
# 产物：target\release\win-pingpang-tab.exe
```

## 已知限制

- 触发键修改须重启程序生效（配置热重载列入 V1.1）；
- 管理员权限窗口在前台时，触发键可能收不到（Windows UIPI 限制；如常与管理员窗口交互，建议以管理员身份运行本程序）；

## 技术实现

Windows 低层鼠标钩子（`WH_MOUSE_LL`）+ 吞放状态机 + `AttachThreadInput` 前台锁绕过。完整设计见 [docs/technical-design.md](docs/technical-design.md)，需求分析见 [docs/requirements-analysis.md](docs/requirements-analysis.md)。

## License

见 [LICENSE](LICENSE)。
