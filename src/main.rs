//! WinPingPangTab：按住配置的组合键 + 鼠标键 → 激活鼠标下方被盖窗口。
//! 启动顺序（技术设计 §1.1）：DPI 声明 → 配置加载 → 日志 → 权限自检 → 单实例 → 线程 → 托盘。
// 无界面工具：windows 子系统，双击启动不弹控制台窗口
// （M1 实测：控制台窗口会作为可切换目标污染层叠栈——曾把自身控制台激活到前台）。
#![windows_subsystem = "windows"]

mod autostart;
mod config;
mod hook;
mod logging;
mod proc_cache;
mod stack;
mod switcher;
mod tray;
mod util;

use std::sync::mpsc;

use tracing::{info, warn};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::hook::TriggerConfig;
use crate::switcher::ActivateRequest;

fn main() {
    // 1. DPI 必须第一行（§4.2）：声明后钩子 pt 与窗口矩形同为物理像素。
    //    结果延后记录——日志尚未初始化，此处 tracing 会丢。
    let dpi_ok = util::set_dpi_awareness();

    // 2. 配置先读（§5）；消息在日志就绪后统一记录（容错不拒启）
    let (cfg, cfg_msgs) = config::load();

    // 3. 日志尽早初始化（级别/大小来自配置，§7）
    logging::init(&cfg.log_level, cfg.log_max_mb * 1024 * 1024);
    for m in &cfg_msgs {
        warn!("配置: {m}");
    }
    if dpi_ok {
        info!("DPI: PerMonitorAwareV2 已声明");
    } else {
        warn!("SetProcessDpiAwarenessContext 失败（多屏混合 DPI 下坐标可能偏移）");
    }

    // 4. 权限自检（M2 补气泡提示与一键自提升）
    let elevated = util::is_elevated();
    info!(
        "WinPingPangTabv{} 启动 | 权限: {}",
        env!("CARGO_PKG_VERSION"),
        if elevated { "管理员" } else { "普通（管理员窗口前台时触发键会失效，详见文档 §4.8）" }
    );

    // 5. 单实例（§4.4）
    let _single = match util::SingleInstance::acquire() {
        Ok(s) => s,
        Err(()) => {
            info!("检测到已有实例在运行，本实例静默退出");
            return;
        }
    };

    // 6. 触发配置：配置字符串 → VK/消息号（解析校验 + 回落，§5.1）
    let trigger = resolve_trigger(&cfg);
    let (tx, rx) = mpsc::channel::<ActivateRequest>();
    hook::wire(trigger, tx);

    // 7. 钩子线程（WH_MOUSE_LL）与工作线程
    let (tid_tx, tid_rx) = mpsc::channel::<u32>();
    let hook_thread = std::thread::Builder::new()
        .name("mouse-hook".into())
        .spawn(move || hook::run(tid_tx))
        .expect("钩子线程创建失败");
    let _worker_thread = std::thread::Builder::new()
        .name("switcher".into())
        .spawn(move || switcher::run(rx))
        .expect("工作线程创建失败");

    let hook_tid = tid_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or(0);
    if hook_tid == 0 {
        warn!("钩子线程 2s 内未就绪（安装失败？），仅托盘退出可用");
    }

    let mods_disp = cfg
        .modifiers
        .iter()
        .map(|m| display_modifier(m))
        .collect::<Vec<_>>()
        .join("+");
    let btn_disp = display_button(&cfg.button);
    info!("WinPingPangTab已就绪：按住 {mods_disp} + {btn_disp} 穿透激活鼠标下方被盖窗口");

    // 8. 主线程 = 托盘消息循环（阻塞直至用户右键托盘选择退出）
    let tip = format!("WinPingPangTab：{mods_disp}+{btn_disp} 穿透切换 | 右键托盘图标打开菜单");
    tray::run(tip);

    // 9. 优雅退出：通知钩子线程 WM_QUIT（其循环退出并卸载钩子），join 后结束
    if hook_tid != 0 {
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                hook_tid,
                windows::Win32::UI::WindowsAndMessaging::WM_QUIT,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
    let _ = hook_thread.join();
    // 工作线程阻塞在 channel recv（Sender 驻留 OnceLock）——随进程结束终止
    info!("WinPingPangTab已退出");
}

/// 配置字符串 → 触发配置（VK/消息号/X 键位）；无法识别的项记日志并回落默认（§5.1）。
fn resolve_trigger(cfg: &config::Config) -> TriggerConfig {
    let mut mods_vk = Vec::new();
    for name in &cfg.modifiers {
        match modifier_vk(name) {
            Some(vk) => mods_vk.push(vk),
            None => warn!("配置: 修饰键 \"{name}\" 无法识别，已忽略"),
        }
    }
    if mods_vk.is_empty() {
        warn!("配置: 有效修饰键为空，回落默认 [lalt]");
        mods_vk.push(VK_LMENU);
    }
    let (bd, bu, xb) = match button_msgs(&cfg.button) {
        Some(t) => t,
        None => {
            warn!(
                "配置: 触发键 \"{}\" 无法识别（right/middle/x1/x2），回落 right",
                cfg.button
            );
            (WM_RBUTTONDOWN, WM_RBUTTONUP, None)
        }
    };
    TriggerConfig {
        modifiers: mods_vk,
        button_down: bd,
        button_up: bu,
        xbutton: xb,
        blacklist: cfg.blacklist.clone(),
    }
}

fn modifier_vk(name: &str) -> Option<VIRTUAL_KEY> {
    Some(match name {
        "lalt" => VK_LMENU,
        "ralt" => VK_RMENU,
        "lctrl" => VK_LCONTROL,
        "rctrl" => VK_RCONTROL,
        "lshift" => VK_LSHIFT,
        "rshift" => VK_RSHIFT,
        "lwin" => VK_LWIN,
        "rwin" => VK_RWIN,
        _ => return None,
    })
}

/// (WM_xBUTTONDOWN, WM_xBUTTONUP, Some(XBUTTON1/2))；right/middle 无键位比对。
fn button_msgs(button: &str) -> Option<(u32, u32, Option<u16>)> {
    Some(match button {
        "right" => (WM_RBUTTONDOWN, WM_RBUTTONUP, None),
        "middle" => (WM_MBUTTONDOWN, WM_MBUTTONUP, None),
        "x1" => (WM_XBUTTONDOWN, WM_XBUTTONUP, Some(1)),
        "x2" => (WM_XBUTTONDOWN, WM_XBUTTONUP, Some(2)),
        _ => return None,
    })
}

fn display_modifier(name: &str) -> &str {
    match name {
        "lalt" => "左Alt",
        "ralt" => "右Alt",
        "lctrl" => "左Ctrl",
        "rctrl" => "右Ctrl",
        "lshift" => "左Shift",
        "rshift" => "右Shift",
        "lwin" => "左Win",
        "rwin" => "右Win",
        _ => name,
    }
}

fn display_button(name: &str) -> &str {
    match name {
        "right" => "右键",
        "middle" => "中键",
        "x1" => "侧键1",
        "x2" => "侧键2",
        _ => name,
    }
}
