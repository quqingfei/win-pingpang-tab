# WinPingPangTab技术设计文档（v0.1 草案，待核对）

对应需求：`docs/requirements-analysis.md`（含 2026-10-22 已确认决策）。
目标版本：**V1.0 MVP** — 左Alt+右键激活鼠标下方第二层被盖窗口，托盘可控，无主窗口。

## 1. 总体架构

### 1.1 线程模型（4 类执行流）

```
┌─────────────────────────────────────────────────────────┐
│ 主线程 (main)                                            │
│  · DPI 声明 → 权限自检(§4.8) → 加载配置 → 单实例检查      │
│  · spawn 钩子/工作线程                                    │
│  · 运行托盘：Shell_NotifyIcon + 隐藏消息窗口 + GetMessage │
│  · 托盘菜单：暂停/恢复、以管理员身份重启*、开机自启动、退出│
│    （*仅普通权限时显示；首次启动气泡提示权限状态，§4.8）   │
└──────────────┬──────────────────────────────────────────┘
               │ 共享状态：AtomicBool(paused) / channel
┌──────────────┴─────────┐  ┌──────────────────────────────┐
│ 钩子线程 (hook)         │  │ 工作线程 (worker)             │
│ · SetWindowsHookExW     │→ │ mpsc recv(point)             │
│   WH_MOUSE_LL           │  │ · EnumWindows 构建该点窗口栈  │
│ · GetMessage 循环       │  │ · 目标=可见顶层之下第一层      │
│ · 回调只做：识别/吞事件 │  │ · AttachThreadInput 前台激活  │
│   /查黑名单缓存/发消息   │  │ · 失败 fallback → 任务栏闪烁  │
│   (微秒级，其余不做)    │  └──────────────────────────────┘
└────────────────────────┘
   （进程名缓存 map 由工作线程惰性维护，钩子线程只读）
```

设计铁律（应对钩子 300ms 超时，`LowLevelHooksTimeout`）：
**钩子回调内禁止任何枚举/激活/IO**。只允许：读 `MSLLSTRUCT`、`GetAsyncKeyState`、查进程名缓存、`channel.send`。全部耗时微秒级。窗口枚举、激活一律在工作线程。

### 1.2 一次切换的完整时序

```
用户按住左Alt → 按下右键
  → WH_MOUSE_LL 回调 (钩子线程)
      1. wParam == WM_RBUTTONDOWN
      2. 配置修饰键集合 GetAsyncKeyState 全部高位为 1？ 否 → CallNextHookEx 放行
      3. paused 标志为 on？ → 放行
      4. 黑名单检查：pt → WindowFromPoint → GA_ROOT → 进程名(缓存)
         命中黑名单 → 放行（该软件自己处理 Alt+右键，如 mstsc）
      5. 吞事件：置 thread_local 状态 swallow_up = true，return 1
      6. channel.send(ActivateRequest { pt: MSLLSTRUCT.pt })   ← 用按下瞬间坐标，不用 GetCursorPos
  → 后续 WM_RBUTTONUP 到达
      swallow_up == true → 吞掉并复位状态；否则放行
  → 工作线程 recv
      7. EnumWindows 构建 pt 处窗口栈（规则见 §3）；翻出记录剪枝（§3）
      8. 目标 = 自第二层起第一个「非前台、非已翻出、非同链路盖层」的窗口（§3）
      9. 无目标但有翻出记录 → 全部收起（放回各自盖层之下）；无记录 → 结束
     10. 跨位置：先收起不在当前栈中的旧翻出窗口（一次触发 = 收旧 + 翻新）
     11. 激活（§4.1 前台锁绕过流程）；失败 → §4.1 fallback；仍失败 → FlashWindow 任务栏闪烁
全程目标 < 10ms，用户无感
```

### 1.3 事件吞放状态机（钩子线程内）

| 事件 | 条件 | 动作 |
|---|---|---|
| 配置键 DOWN | 配置修饰键全部按下 & 未暂停 & 非黑名单 | 吞（return 1），置 swallow_up，发 ActivateRequest |
| 配置键 DOWN | 其余情况 | 放行（CallNextHookEx），清 swallow_up |
| 配置键 UP | swallow_up == true | 吞，清 swallow_up |
| 配置键 UP | swallow_up == false | 放行 |
| 其他所有事件 | — | 永远放行 |

吞 down 必须连带吞 up，否则目标窗口会收到"孤儿抬起"，可能触发其内部拖拽/右键逻辑。

## 2. 模块划分

```
src/
├── main.rs        入口：DPI/单实例/配置 → spawn 线程 → 托盘消息循环
├── config.rs      TOML 加载、默认值、黑名单集合（HashSet，进程名小写）
├── hook.rs        钩子线程：安装/卸载、回调、状态机、黑名单放行、channel send
├── stack.rs       窗口栈构建：EnumWindows 过滤 + 点命中 + 目标选择（纯逻辑，可单测）
├── switcher.rs    工作线程：recv → 选目标 → 激活（前台锁绕过 + fallback）；翻出状态/收起还原
├── tray.rs        托盘图标 + 菜单（暂停/恢复、开机自启动勾选、退出）
├── autostart.rs   HKCU\...\Run 注册表读写
├── proc_cache.rs  (hwnd_root → 进程名) 惰性缓存；重查询仅当缓存 miss
└── util.rs        错误类型、单实例命名互斥体、路径（%APPDATA%\win-pingpang-tab\）
```

依赖方向：`hook → proc_cache`，`switcher → stack → proc_cache`，`main → 全部`。`stack.rs` 的几何/排序逻辑不触碰 Win32 的部分抽成纯函数以便单测。

## 3. 窗口栈构建算法（stack.rs）

`EnumWindows` 按现状 Z 序**从顶到底**回调，逐个过滤，命中 pt 的入栈：

| 过滤条件 | API | 理由 |
|---|---|---|
| 不可见 → 跳过 | `IsWindowVisible` | |
| 最小化 → 跳过 | `IsIconic` | 最小化矩形在 (-32000,-32000) |
| UWP 挂起 cloaked → 跳过 | `DwmGetWindowAttribute(DWMWA_CLOAKED) != 0` | 否则激活出"隐形窗" |
| 系统类名黑名单 → 跳过 | 类名 ∈ {Progman, WorkerW, Shell_TrayWnd, Shell_SecondaryTrayWnd} | 桌面/任务栏不可作为目标 |
| 自身进程窗口 → 跳过 | `GetWindowThreadProcessId == GetCurrentProcessId` | 自身窗口（含 M2 托盘隐藏窗口）永不作为目标。M1 实测：双击启动的控制台窗口被激活到前台（污染层叠栈），故同时改用 windows 子系统免控制台启动 |
| 取可见边界矩形 | `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`，失败退 `GetWindowRect` | 阴影区域不算命中 |

目标选择规则（2026-10-23 引入翻出状态；含 2026-10-22 M1 前台跳过修正）：

```
stack  = 命中 pt 的窗口（按 Z 序，前=顶，过滤后）
raised = 工作线程翻出记录 [(target, cover)]（翻出顺序；cover = 翻出时被穿透的栈顶盖层）

每轮先剪枝：target/cover 已销毁、target 被最小化、target 被用户手动盖回
（Z 序低于 cover，GetWindow(GW_HWNDPREV) 遍历判定）→ 移除记录

目标 = 自 stack[1] 起第一个同时满足的层：
  · != 当前前台窗口（激活前台无可见变化，M1 修正）
  · ∉ 已翻出层——已在最前，继续下挖更深层
  · ∉ 同链路盖层——「cover ∈ raised 且其 target 仍在当前栈中」的 cover：
    重新翻盖层 = 把同链路已翻出窗口盖回（乒乓回退）；
    其 target 不在当前栈（跨位置）时，该盖层可作为新目标正常翻出

无目标：有翻出记录 → 全部收起（按翻出顺序放回各自 cover 之下，还原原 Z 序）；
       无记录 → 无操作（纯桌面/仅一层）
有目标：先收起「target 不在当前栈中」的旧翻出记录，再激活目标；
       成功后记录 (target, 本轮栈顶盖层)
```

交互语义（用户确认）：
- **同位置逐层下挖**：栈 [T, A, B] 连续触发 → 依次翻出 A、B，已翻出层保持显示不被盖回；
- **无目标收起 = 显示/隐藏乒乓**：常规两窗 [T, A] 场景连按，表现为 A 翻出 ↔ 收起交替；
- **跨位置先收后翻（2026-10-23）**：换位置触发且新位置有目标 → **一次触发即收起旧位置
  翻出窗口并翻出新目标**（修正前：先收起、需再按一次才翻新）。「旧位置」判定 = 翻出窗口
  不在当前栈中；旧翻出窗口仍压在当前点上则视为同链路，保持显示继续下挖；
- **手动干预自动失效**：用户点回盖层 / 最小化 / 关闭翻出窗口 → 剪枝清除对应记录，
  后续触发重新翻出。

前台跳过的修正原因（M1 实测，2026-10-22）：钉钉等 IM 的置顶迷你悬浮窗压在前台窗口上时，
旧规则"永远取 stack[1]"取到的是前台窗口本身——激活前台无任何可见变化，且日志误报
"切换成功"（M1 现场日志：连续 8 次触发目标恒为前台进程）。

**发布形态**：`#![windows_subsystem = "windows"]` 无控制台启动——控制台窗口是真实可见窗口，会进入层叠栈成为切换目标（M1 实测：自身控制台被激活、用户从 Windows Terminal 启动时终端被激活）。

已知取舍（记录在案）：
- 鼠标悬停在**非前台**顶层窗口上时，激活的是它下面那层而非它本身——符合"穿透第一层"的直觉，保持规则统一；
- 窗口几何（位置/尺寸/Z 序）永远实时查询、无缓存；翻出状态仅记录 (target, cover) 句柄对，不含几何信息，跨屏拖动/重排后无脏数据；
- 旧盖层（曾穿透过的盖层）在其翻出窗口仍在当前点时不可被翻出（防乒乓），跨位置后可正常翻出。

## 4. 关键技术点实现方案

### 4.1 前台锁绕过（switcher.rs）

Windows 默认拒绝后台进程 `SetForegroundWindow` 抢焦点（只闪任务栏）。采用两级方案：

```
主方案（AttachThreadInput 手法，AHK 社区长期验证）：
  fg        = GetForegroundWindow()
  fg_tid    = GetWindowThreadProcessId(fg)
  me_tid    = GetCurrentThreadId()
  AttachThreadInput(me_tid, fg_tid, TRUE)
  BringWindowToTop(target)
  ok        = SetForegroundWindow(target)
  AttachThreadInput(me_tid, fg_tid, FALSE)          // 无论成败必须 detach

fallback（主方案失败时）：
  keybd_event(VK_MENU, ...)     // 模拟一次系统键，解除本进程前台锁定
  SetForegroundWindow(target)
  再失败 → FlashWindowEx(target) 任务栏闪烁，日志记录，放弃
```

细节：
- 若 fg == target（理论不会发生）直接返回；
- 目标线程未响应（`IsHungAppWindow`）→ 仍尝试，失败走闪烁。

### 4.2 DPI 与多显示器（main.rs 第一行）

**多显示器：天然支持，无需特殊处理。**
Windows 对鼠标坐标与窗口矩形统一使用"虚拟屏幕"坐标系——所有屏幕拼成一个大平面，副屏坐标可为负。本工具的核心判断是"点是否落在窗口矩形内"的纯几何运算，与屏幕数量、排列方式（任意上下左右组合、主屏居中）、窗口在哪块屏无关；窗口横跨两屏的情形也自然正确（本质是一个大矩形）。

**混合 DPI（如主屏 100% + 副屏 150%）是唯一的坑，启动即消除：**
若进程不声明 DPI 感知，Windows 会对坐标做虚拟化缩放，导致钩子拿到的 pt 与 `EnumWindows` 返回的矩形不在同一坐标系，命中判断偏移（典型症状：副屏打不中、跨屏边缘错位）。

对策：进程启动第一行调用
`SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`。
声明后全进程坐标均为物理像素，全局一致，**零换算、零分屏特判**。

验收依赖 §8.2 清单第 6 条（混合 DPI 双屏跨屏边缘命中）。

### 4.2.1 场景分析：跨屏拖动窗口后，位置会不会"失效"？

**结论：不存在"记录的位置"，因此无从失效。** 本设计没有窗口位置缓存（需求分析 §5.2 的核心决策）——每次热键触发都实时 `EnumWindows` 读取矩形，读到的永远是"此刻"的位置：

| 时机 | 行为 |
|---|---|
| 拖动完成后按热键 | 读到新屏上的新矩形，按新位置命中 |
| 混合 DPI 跨屏（100%↔150%） | 过屏时系统重缩放窗口，矩形实时更新，读到的总是新值 |
| 窗口横跨两屏 | 就是一个大矩形，命中判断不受影响 |
| 拖动途中按热键（需按住左键拖动同时按 Alt+右键，物理上几乎不构成真实操作） | 读到拖动中的实时矩形；栈顶为被拖窗本身，激活其下一层，无害 |

进程名缓存（§4.3）是本设计唯一的缓存（root hwnd → 进程名），与位置/尺寸无关，跨屏拖动不影响其正确性。

**反证（当初否决"记录位置"方案的原因之一）**：若快照记录位置，拖动、贴边 Snap、Win+方向键、分辨率/缩放变更、最小化后恢复、关闭重开（hwnd 变更）都会使记录过期，跨屏拖动恰是重灾区；保鲜需对全部窗口常驻 `EVENT_OBJECT_LOCATIONCHANGE`/`EVENT_SYSTEM_REORDER` 事件钩子，换来持续开销与"事件-查询"竞态。实时查询方案在这类场景上是零成本、零失效。

### 4.3 黑名单与进程名缓存（proc_cache.rs + hook.rs）

- 缓存结构：`Mutex<HashMap<isize, String>>`（root hwnd → 小写进程名）；
- 查询链（仅缓存 miss 时执行一次）：`WindowFromPoint` → `GetAncestor(GA_ROOT)` → `GetWindowThreadProcessId` → `QueryFullProcessImageNameW`（进程已退出则从缓存清除，下次重查）；
- 默认黑名单：`mstsc.exe`（远程桌面，Alt+右键需原样放行给远端处理）。用户可在配置中增删；
- 每次鼠标事件最多一次缓存查找（HashMap，纳秒级），无超时风险；
- 窗口销毁导致 hwnd 复用错配概率极低，且后果仅是"多放行一次右键"，可接受。

### 4.4 单实例（util.rs）

`CreateMutexW(L"Local\\WinPingPangTab.SingleInstance")`；`ERROR_ALREADY_EXISTS` → 写日志后静默退出（不做弹窗，保持无界面）。唯一例外：§4.8 自提升交接——新实例获取互斥体失败时重试 ≤3s，等待旧实例释放。

### 4.5 开机自启动（autostart.rs，托盘菜单勾选式）

注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 写 `WinPingPangTab= "<exe 全路径>"`，取消勾选则删值。不需要管理员权限。

### 4.6 优雅退出

托盘"退出" → 置退出标志 → `UnhookWindowsHookEx` → `PostThreadMessage(WM_QUIT)` 到钩子线程 → 主循环退出 → 进程结束。崩溃兜底：钩子属于进程，进程退出自动脱钩，不会残留系统级副作用。

### 4.7 钩子看门狗（hook.rs）

工作线程每次处理消息时给钩子线程 `PostMessage` 一个自检消息；连续 N 分钟收不到应答 → 记录错误日志（V1 不自动重挂，留 V1.1）。

### 4.8 权限自检与自提升（main.rs / tray.rs，2026-10-22 需求补充）

**事实澄清（"功能是否需要管理员"的结论）**：核心功能（LL 钩子、EnumWindows、激活普通窗口）**不需要管理员**，普通权限即可覆盖日常 IDE/IM 场景。UIPI 仅带来两个受限场景：
1. 管理员窗口为前台/位于鼠标下时，普通权限进程的 LL 钩子**收不到对应输入事件** → 触发键静默失效；
2. `SetForegroundWindow` 激活管理员窗口被拒（§4.1 fallback 仍失败 → 闪烁）。

以管理员运行可同时消除这两个限制。

**行为设计**（不强制管理员）：
- **启动自检**：`OpenProcessToken(GetCurrentProcess())` → `GetTokenInformation(TokenElevation)` 判断是否提升；结果写日志，托盘 tooltip 注明（管理员运行追加"(管理员)"）；
- **普通权限启动时**：托盘气泡提示（`Shell_NotifyIcon NIF_INFO`）**仅首次**（`%APPDATA%` 状态文件防重复打扰），措辞如实：
  "已以普通权限启动。日常使用（IDE/IM 切换）无需管理员；但管理员权限的窗口（如任务管理器）在前台时触发键会失效、也无法切换到它们。如需完整支持，请右键托盘图标选择【以管理员身份重启】。"
- **托盘菜单【以管理员身份重启】**（仅普通权限时显示）：`ShellExecuteExW(runas)` 触发 UAC：
  - 同意 → 新进程（elevated）启动，旧进程释放单实例互斥体后退出；新实例单实例检查失败时重试 ≤3s（交接窗口，见 §4.4）；
  - 拒绝 → 旧进程继续普通权限运行，日志 warn，不再弹气泡；
- **配置 `[privilege] require_admin`**（默认 false）：true 时启动即检测，非管理员 → 气泡提示后立即自提升；UAC 被拒绝 → 退出进程（日志说明）。

**配套限制**：`require_admin = true` 与开机自启动（HKCU Run）组合时每次登录弹一次 UAC（HKCU Run 无法静默提权）→ 建议二者不同时开启；计划任务静默提权方案列 V1.1。

**里程碑归属**：M1 记录日志级自检结果（无托盘）；气泡提示、一键自提升、require_admin 随 M2 托盘/配置落地。

### 4.9 托盘与图标（tray.rs，退出能力已提前至 M1）

托盘右键菜单（2026-10-22 实现）：**【配置文件】**（生成并打开 config.toml，无关联程序时记事本兜底）/ **【开机自启动】**（勾选式，HKCU Run，见 §4.5）/ 分隔线 / **【退出】**。悬浮提示依当前配置动态生成（如"左Alt+右键"）。暂停、管理员重启随后续版本接入。

托盘实现：隐藏消息窗口 + `Shell_NotifyIconW`；主线程消息循环即托盘循环，退出时删图标 → 通知钩子线程 WM_QUIT → join。

托盘图标加载链（用户自定义按此顺序覆盖，重启生效）：

| 优先级 | 来源 | 说明 |
|---|---|---|
| 1 | exe 同目录 `icon.ico` | 便携使用友好 |
| 2 | `%APPDATA%\win-pingpang-tab\icon.ico` | 写权限永远可用（推荐位置） |
| 3 | 内嵌默认图标 | `assets/icon.png`（32×32，托盘标准小图标尺寸，覆盖 100%~200% DPI）经 `include_bytes!` 编译进二进制，启动时 PNG 解码 → `CreateIconIndirect`，无需资源编译器 |
| 4 | 系统默认 `IDI_APPLICATION` | 兜底 |

- 自持图标句柄（来源 1~3）退出时 `DestroyIcon`，系统共享图标（来源 4）不销毁；
- exe 文件图标（Explorer 显示）由 `build.rs` + `winresource` 嵌入 `assets/icon.ico`（windres，尽力而为：资源编译器缺失时仅 cargo:warning 不阻断构建，托盘图标不受影响）；
- 日志记录实际加载来源，排查"图标不对"直接看日志。

## 5. 配置文件（%APPDATA%\win-pingpang-tab\config.toml）

**设计原则（2026-10-22 需求确认：通用/默认配置统一置于配置文件）**：
1. **单一来源**：所有用户可调项只放这一个文件——触发键位、进程黑名单、权限策略、日志级别与大小上限；不散落注册表/多文件（两个例外：开机自启动是托盘勾选项存 HKCU Run，见 §4.5；自定义图标走 `icon.ico` 文件约定，见 §4.9）；
2. **文件即文档**：首次运行自动生成带完整中文注释的默认配置（见下方），用户无需查手册；
3. **容错不拒启**：任何字段缺失/非法 → 记 error 日志 + 回落该字段默认值，进程绝不因此启动失败；
4. **重启生效**：V1 修改后重启进程生效，热重载列 V1.1。

```toml
# 生成默认配置：首次运行自动写出（带注释）

[trigger]
# 修饰键：lalt/ralt/lctrl/rctrl/lshift/rshift/lwin/rwin 任意组合（触发时须全部按住）
# 至少 1 个；触发鼠标键：right / middle / x1 / x2（不提供 left）
modifiers = ["lalt"]
button = "right"

[blacklist]
# 命中（鼠标下最顶窗口属于这些进程）时触发键原样放行
processes = ["mstsc.exe"]

[privilege]
# true：启动强制管理员——非管理员时提示并自动自提升（UAC 拒绝则退出）
# false（默认）：普通权限运行 + 首次气泡提示 + 托盘菜单【以管理员身份重启】(§4.8)
require_admin = false

[log]
# "off" | "error" | "info" | "debug"；默认 info（保证出问题时有现场）
# 单文件 app.log，超过 max_size_mb 自动清空重写（默认 10）
level = "info"
max_size_mb = 10
```

**配置项总览（全量速查）**：

| 节 | 项 | 默认值 | 说明 | 详见 |
|---|---|---|---|---|
| `[trigger]` | `modifiers` | `["lalt"]` | 修饰键组合（lalt/ralt/lctrl/rctrl/lshift/rshift/lwin/rwin，触发时须全部按住，至少 1 个） | §5.1 |
| `[trigger]` | `button` | `"right"` | 触发鼠标键（right / middle / x1 / x2，无 left） | §5.1 |
| `[blacklist]` | `processes` | `["mstsc.exe"]` | 命中进程（鼠标下最顶窗口归属）时触发键原样放行；不区分大小写 | §4.3 |
| `[privilege]` | `require_admin` | `false` | 是否强制管理员启动（UAC 拒绝则退出） | §4.8 |
| `[log]` | `level` | `"info"` | off / error / info / debug | §7 |
| `[log]` | `max_size_mb` | `10` | 单文件上限（MB），超限清空重写 | §7 |

**落地状态（2026-10-22）**：[trigger]/[blacklist]/[log] 已接入（托盘「配置文件」菜单项随此交付）；触发键支持 right/middle/x1/x2（X 键比对 mouseData 高字区分 XBUTTON1/2）；[privilege] 暂未生成在模板中，随自提升功能接入。

后续新增可调项一律进此文件（遵循上述 4 条原则）；不得新增注册表值/环境变量/命令行参数等旁路配置。

### 5.1 触发键自定义（V1 支持，重启进程生效）

**原理与成本**：钩子用 `GetAsyncKeyState` 即时查询修饰键、回调按消息类型匹配鼠标键，而非 `RegisterHotKey` 注册式热键——**改键无需重挂钩子**，仅是回调读取配置驱动的修饰键集合与目标鼠标键消息，钩子回调耗时不变（微秒级）。

**实现要点**（hook.rs）：
- 修饰键集合：`Vec<VK_*>`（加载配置时解析并校验，触发判定 = 集合内全部 `GetAsyncKeyState` 高位为 1，且集合外修饰键**不要求**状态）；
- 触发键消息映射：`right→WM_RBUTTONDOWN/UP`、`middle→WM_MBUTTONDOWN/UP`、`x1/x2→WM_XBUTTONDOWN/UP`（检查 wParam 的 XButton 位）；
- §1.3 状态机对触发键参数化：吞 down/up 的对象从"右键"换成"配置键"。

**配置校验规则**（非法 → 记 error 日志 + 回落默认 左Alt+右键）：
1. `modifiers` 至少 1 个——禁止裸鼠标键触发（否则普通右键全被吞）；
2. `button` ∉ {left, ""}——左键不做触发键（修饰+左键与浏览器/IDE 选择、拖拽冲突过重）；
3. 未知修饰键/按钮名 → 回落默认并日志提示；
4. 未配置 `[trigger]` 节 → 使用默认值。

**生效时机**：V1 重启进程生效（不做热重载）；热重载/托盘"重载配置"列为 V1.1。

## 6. Cargo 依赖

```toml
[dependencies]
windows = { version = "*", features = [
    "Win32_Foundation",                   # POINT/RECT/BOOL
    "Win32_UI_WindowsAndMessaging",       # SetWindowsHookExW/EnumWindows/SetForegroundWindow/GetAsyncKeyState...
    "Win32_Graphics_Dwm",                 # DwmGetWindowAttribute
    "Win32_Graphics_Gdi",                 # GetWindowRect 辅助/窗口类型
    "Win32_System_Threading",             # OpenProcess/AttachThreadInput 相关
    "Win32_Security",                    # 进程访问权限
    "Win32_UI_Shell",                     # Shell_NotifyIcon（若手写托盘）
    "Win32_System_Registry",              # 自启动注册表
] }
serde = { version = "1", features = ["derive"] }
toml = "0.8"
crossbeam-channel = "0.5"                 # hook→worker 消息（mpsc 即可，选型简单优先 std；此为候选，实现时定）
tracing = "0.1"
tracing-appender = "0.2"                  # non_blocking 写缓冲 + 自定义 MakeWriter（10MB 超限清空，§7）
tray-item = "0.10"                        # 托盘 + 右键菜单（若菜单能力不足，备选 tray-icon）

[profile.release]
lto = true
strip = true
opt-level = "s"
```

`windows` 具体版本以 crates.io 最新稳定为准；feature 缺漏按编译提示补。

## 7. 日志与错误处理

**日志（V1 默认开启，用于事后排查）**
- `tracing` + 单文件 `%APPDATA%\win-pingpang-tab\logs\app.log`，默认 `level=info`——出问题无需事先调配置就有现场可查；
- info 级记录：启动/退出、配置加载结果（含非法回落）、钩子安装/摘除、每次切换（命中点、目标窗口句柄/进程名、激活结果）、激活失败及原因；排查"切错窗口"可临时调 debug（含该点完整窗口栈）；
- **大小上限 10MB，超限清空重写**（需求指定）：自定义 `MakeWriter` 维护已写字节计数（AtomicU64），写前检查，超过 `max_size_mb`（默认 10，可配置）即 `File::set_len(0)` 截断后继续顺序写。单文件、不滚动归档，实现最简且磁盘占用恒定；
  - 截断语义：写前检查意味着文件短暂超限至 max + 最后一条（~200B，0.002%），换来**最后一条日志永远完整**；
- 日志级别可配置：`off / error / info / debug`；
- **铁律延伸：钩子回调内禁止任何 IO（含日志）**——回调如需留痕，错误信息随 channel 消息交工作线程落盘；钩子安装/卸载发生在钩子生效前后，允许直接记日志。

**错误策略**：Win32 调用失败不 panic，一律 `Result` + 日志 + 降级（激活失败→闪烁，枚举失败→本次不切换）。工具定位是"失联不如慢"，任何单次失败不影响后续触发。

## 8. 测试计划

### 8.1 单元测试（不依赖真实窗口系统）

- `stack.rs`：构造假窗口数据（z 序 + 矩形 + 属性标记），验证过滤规则与"栈[1] 选择"、点在边界（含/不含）行为；
- `config.rs`：缺省配置生成、非法字段容错、黑名单大小写归一、`[log]` 级别/上限校验；
- `logging`：超限截断逻辑（临时目录 + 小阈值，验证超限清空后可继续写入）。

### 8.2 手动验收清单（发版前全过）

| # | 场景 | 期望 |
|---|---|---|
| 1 | IDE 最大化 + IM 部分被盖，鼠标在 IM 可见区域按 Alt+右键 | IM 置前且获键盘焦点 |
| 2 | IM 被 IDE **完全盖住**，鼠标在 IM 矩形内任一点 | 仍能切换（穿透核心能力） |
| 3 | 同位置连按 10 次 | 严格两窗乒乓，无跳变到第三窗 |
| 4 | 纯右键（不按 Alt）在桌面/文件夹/浏览器 | 行为与系统原生完全一致（零干扰） |
| 5 | mstsc 全屏远程桌面内 Alt+右键 | 原样放行，远端收到右键 |
| 6 | 混合 DPI 双屏（100%+150%），跨屏边缘处命中 | 坐标不偏移，命中正确窗口 |
| 7 | UWP（设置/计算器）挂后台后再切 | 不激活"隐形 cloaked 窗"，切的是真实窗口 |
| 8 | 有窗口最小化的位置 | 最小化窗不被激活 |
| 9 | 普通权限下切管理员终端（任务管理器） | 允许失败→任务栏闪烁（已确认接受） |
| 10 | 按住 Alt+右键快速连点 / 拖动 | 无卡顿、无右键菜单误弹 |
| 11 | 托盘：暂停后 Alt+右键 | 完全放行 |
| 12 | 开机自启动勾选重启、双开第二实例 | 注册表生效；第二实例静默退出 |
| 13 | 鼠标移动时 CPU 占用 | 钩子线程 ≈ 0，无可感知延迟 |
| 14 | 窗口从 100% 屏拖到 150% 屏后立即 Alt+右键（含贴边 Snap、Win+方向键后） | 按拖动后的实时位置命中，无"旧位置"残留失效 |
| 15 | 改配置触发键（如 lctrl+x1）后重启 | 新键生效，原 左Alt+右键 完全放行；非法配置（空 modifiers / button=left / 未知键名）回落默认并记日志 |
| 16 | 正常使用后查看日志：每次切换均有记录（命中点/目标进程/结果）；将 `max_size_mb` 临时调小（如 1）后高频触发 | 有现场可排查；写满上限后自动清空并继续记录，磁盘占用不增长 |
| 17 | 普通权限启动 | 日志记录权限状态；首次气泡提示（措辞如实，见 §4.8）；托盘菜单出现【以管理员身份重启】 |
| 18 | 托盘【以管理员身份重启】 | UAC 同意 → 新实例以管理员运行（tooltip 带标记，可切任务管理器），旧实例退出；拒绝 → 继续普通权限运行 |
| 19 | `require_admin=true` 时非管理员启动 | 提示后立即自提升；UAC 拒绝 → 进程退出且日志说明 |
| 20 | A 处翻出窗口后，移到 B 处（有自己的被盖窗口）触发 | **一次触发**：A 处窗口收回原盖层之下 + B 处目标翻出（跨位置先收后翻，§3） |

## 9. 里程碑

| 阶段 | 内容 | 验收 |
|---|---|---|
| M1 核心跑通 | 钩子线程 + 吞放状态机 + 枚举栈 + 激活流程 + 基础日志（info 单文件 10MB 超限清空，§7），配置硬编码 | 通过清单 1~4、10 |
| M2 可用性完备 | 配置文件 + 触发键自定义（校验/回落，§5.1）+ 黑名单缓存 + 托盘（暂停/退出）+ 单实例 + 权限自检/气泡提示/一键自提升/require_admin（§4.8） | 加过 5、7、8、11、12、15、17~19 |
| M3 收尾发布 | 自启动勾选、看门狗、release 优化、README 发布说明 | 全清单 + 资源占用达标 |

> 日志归属：M1 落地基础日志（info + 10MB 清空，硬编码）；M2 随配置文件一起将级别/上限配置化（`[log]` 节）。

## 10. 已知限制（V1 明确不做，防预期错位）

1. 触发键可自定义（§5.1），但**修改须重启进程生效**（无热重载）；
2. 无滚轮逐层轮换、无切换可视化描边（V1.1 候选）；
3. 默认普通权限运行：管理员窗口前台时触发键失效、也无法切换到管理员窗口（§4.8 自检提示 + 托盘一键自提升可消除；`require_admin=true` 可强制管理员）；`require_admin=true` 与开机自启动同开时每次登录弹 UAC，静默提权方案列 V1.1；
4. 黑名单粒度 = 进程名，不支持窗口类名/标题规则；
5. 无安装器，绿色单 exe + zip。
