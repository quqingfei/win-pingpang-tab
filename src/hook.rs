//! 低层鼠标钩子线程（技术设计 §1.1/§1.3/§4.3）。
//!
//! 铁律：回调内禁止枚举/激活/IO（含日志），只允许——
//! 读 MSLLPSTRUCT、GetAsyncKeyState、查进程名缓存、channel.send。全部微秒级。
//! 吞 down 必须连带吞 up（防"孤儿抬起"）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use tracing::{error, info};
use windows::Win32::Foundation::{LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL,
    MSLLHOOKSTRUCT, MSG,
};

use crate::proc_cache;
use crate::switcher::ActivateRequest;

/// 触发配置（main 依 config.toml 解析注入，§5.1）。
pub struct TriggerConfig {
    pub modifiers: Vec<windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY>,
    pub button_down: u32,
    pub button_up: u32,
    /// Some(1)=XBUTTON1 / Some(2)=XBUTTON2：X 键共用消息号，须比对 mouseData 高字
    pub xbutton: Option<u16>,
    pub blacklist: Vec<String>,
}

static CONFIG: OnceLock<TriggerConfig> = OnceLock::new();
static SENDER: OnceLock<Sender<ActivateRequest>> = OnceLock::new();
static PAUSED: AtomicBool = AtomicBool::new(false);
static SWALLOW_UP: AtomicBool = AtomicBool::new(false);

/// 安装钩子前注入：触发配置 + 激活请求通道。
pub fn wire(config: TriggerConfig, sender: Sender<ActivateRequest>) {
    let _ = CONFIG.set(config);
    let _ = SENDER.set(sender);
}

/// 暂停/恢复（托盘控制，M1 无界面恒为 false；M2 托盘接入）。
#[allow(dead_code)]
pub fn set_paused(paused: bool) {
    PAUSED.store(paused, Ordering::SeqCst);
}

/// 钩子线程主体：安装 WH_MOUSE_LL → 报告线程 id → 消息循环驱动回调 → WM_QUIT 时卸载。
/// 返回值通道用于向主线程回报钩子线程 id（优雅退出用）。
pub fn run(report_tid: Sender<u32>) {
    unsafe {
        let hook = match SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) {
            Ok(h) => h,
            Err(e) => {
                error!("安装低层鼠标钩子失败: {e}");
                return;
            }
        };
        info!("低层鼠标钩子已安装 (tid={})", GetCurrentThreadId());
        let _ = report_tid.send(GetCurrentThreadId());

        let mut msg = MSG::default();
        // GetMessageW 返回 BOOL：0=WM_QUIT，-1=错误，均退出（避免死循环刷错误日志）
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 || r.0 == -1 {
                break;
            }
        }

        let _ = UnhookWindowsHookEx(hook);
        info!("低层鼠标钩子已卸载，钩子线程退出");
    }
}

/// WH_MOUSE_LL 回调：吞放状态机（技术设计 §1.3）。
unsafe extern "system" fn hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM_,
) -> LRESULT {
    // 系统要求：code < 0 必须直通
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let Some(cfg) = CONFIG.get() else {
        return CallNextHookEx(None, code, wparam, lparam);
    };
    let msll = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let msg = wparam.0 as u32;

    // X 键消息不区分键位（XBUTTON1/2 共用消息号），须比对 mouseData 高字
    let x_match = match cfg.xbutton {
        None => true,
        Some(x) => ((msll.mouseData >> 16) & 0xFFFF) as u16 == x,
    };
    let is_down = msg == cfg.button_down && x_match;
    let is_up = msg == cfg.button_up && x_match;

    if is_down {
        if !modifiers_all_pressed(cfg) || PAUSED.load(Ordering::SeqCst) {
            SWALLOW_UP.store(false, Ordering::SeqCst);
            return CallNextHookEx(None, code, wparam, lparam);
        }
        let pt = msll.pt;
        // 黑名单：鼠标下最顶窗口归属进程命中则原样放行（该软件自己处理组合键）
        if is_blacklisted(cfg, pt) {
            SWALLOW_UP.store(false, Ordering::SeqCst);
            return CallNextHookEx(None, code, wparam, lparam);
        }
        // 触发：吞 down + 发激活请求（pt 为按下瞬间坐标）
        SWALLOW_UP.store(true, Ordering::SeqCst);
        if let Some(sender) = SENDER.get() {
            let _ = sender.send(ActivateRequest { pt });
        }
        return LRESULT(1); // 吞事件：目标程序不收到右键 down
    }

    if is_up {
        // 连带吞 up：防目标程序收到"孤儿抬起"
        if SWALLOW_UP.swap(false, Ordering::SeqCst) {
            return LRESULT(1);
        }
        return CallNextHookEx(None, code, wparam, lparam);
    }

    CallNextHookEx(None, code, wparam, lparam)
}

fn modifiers_all_pressed(cfg: &TriggerConfig) -> bool {
    cfg.modifiers.iter().all(|vk| {
        let s = unsafe { GetAsyncKeyState(vk.0 as i32) };
        (s as u16) & 0x8000 != 0
    })
}

fn is_blacklisted(cfg: &TriggerConfig, pt: POINT_) -> bool {
    unsafe {
        let top = WindowFromPoint_(pt);
        if top.0.is_null() {
            return false;
        }
        let root = proc_cache::root_of(top);
        match proc_cache::process_name(root) {
            Some(name) => cfg.blacklist.iter().any(|b| b.eq_ignore_ascii_case(&name)),
            None => false,
        }
    }
}

// —— 局部类型别名：减少长路径（保持实现可读）——
type LPARAM_ = windows::Win32::Foundation::LPARAM;
type POINT_ = windows::Win32::Foundation::POINT;
type HWND_ = windows::Win32::Foundation::HWND;

#[allow(non_snake_case)]
unsafe fn WindowFromPoint_(pt: POINT_) -> HWND_ {
    windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(pt)
}

// 其他所有消息（移动/滚轮/左/中/侧键等）不经任何匹配分支 → 走 CallNextHookEx 放行（§1.3）
