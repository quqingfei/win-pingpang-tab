//! 工作线程（技术设计 §1.1/§4.1）：接收激活请求，构建窗口栈，
//! 选择第二层，AttachThreadInput 绕过前台锁激活；失败降级闪烁。

use std::sync::mpsc::Receiver;

use tracing::{debug, info, warn};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYBD_EVENT_FLAGS, VK_MENU, KEYEVENTF_KEYUP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, FlashWindowEx, GetForegroundWindow, GetWindowThreadProcessId,
    SetForegroundWindow, FLASHWINFO, FLASHW_ALL,
};

use crate::proc_cache;
use crate::stack;

/// 钩子线程 → 工作线程的激活请求：使用按下瞬间的 pt（不用事后 GetCursorPos，防打偏）。
pub struct ActivateRequest {
    pub pt: POINT,
}

/// 工作线程主循环。M1 硬编码：触发键 左Alt+右键，黑名单 mstsc.exe（在钩子侧放行）。
pub fn run(receiver: Receiver<ActivateRequest>) {
    info!("工作线程已启动");
    for req in receiver {
        handle(req);
    }
    info!("工作线程退出（channel 已关闭）");
}

fn handle(req: ActivateRequest) {
    let fg = unsafe { GetForegroundWindow() };
    let fg_root = proc_cache::root_of(fg);
    let hit = stack::build_stack(req.pt);
    debug!("pt=({}, {}) 窗口栈(顶→底): {:?}", req.pt.x, req.pt.y, hit);

    // 目标：跳过最顶可见层（被穿透层），从第二层起取第一个非前台层（§3 规则）
    let Some(target_raw) = stack::pick_target(&hit, fg_root.0 as isize) else {
        info!(
            "未切换: pt=({},{}) | 前台={} | 顶下无其他非前台窗口 | 栈(顶→底)=[{}]",
            req.pt.x,
            req.pt.y,
            layer_name(fg_root),
            layer_names(&hit)
        );
        return;
    };
    let target = proc_cache::root_of(HWND(target_raw as _));
    let target_name = layer_name(target);
    let fg_name = layer_name(fg_root);

    let ok = activate(target);
    if ok {
        info!(
            "切换成功: pt=({},{}) | {fg_name} → {target_name} (hwnd=0x{target_raw:X})"
        , req.pt.x, req.pt.y);
    } else {
        warn!(
            "切换失败(已闪烁): pt=({},{}) | {fg_name} → {target_name} (hwnd=0x{target_raw:X})",
            req.pt.x,
            req.pt.y
        );
    }
}

/// 单个窗口（root）的进程名。
fn layer_name(root: HWND) -> String {
    proc_cache::process_name(root).unwrap_or_else(|| "<未知>".into())
}

/// 栈各层进程名，诊断日志用（顶→底，" > " 分隔）。
fn layer_names(stack: &[isize]) -> String {
    stack
        .iter()
        .map(|&h| layer_name(proc_cache::root_of(HWND(h as _))))
        .collect::<Vec<_>>()
        .join(" > ")
}

/// 激活目标窗口（技术设计 §4.1）：主方案 AttachThreadInput，fallback 模拟系统键，仍失败闪烁。
/// 返回是否成功置前。
fn activate(target: HWND) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg == target {
            return true; // 已是前台（理论不会发生）
        }

        // 主方案：附加到当前前台线程的输入队列，解除本进程前台锁
        let me_tid = GetCurrentThreadId();
        let mut attached = false;
        let fg_tid = if fg.0.is_null() { 0 } else { GetWindowThreadProcessId(fg, None) };
        if fg_tid != 0 {
            if AttachThreadInput(me_tid, fg_tid, true).as_bool() {
                attached = true;
            } else {
                debug!("AttachThreadInput 失败，直接尝试置前");
            }
        }

        let _ = BringWindowToTop(target);
        let mut ok = SetForegroundWindow(target).as_bool();

        if attached {
            // 无论成败必须 detach
            let _ = AttachThreadInput(me_tid, fg_tid, false);
        }

        // 验证：有些系统版本 SetForegroundWindow 返回成功但未生效
        ok = ok && GetForegroundWindow() == target;

        if !ok {
            // fallback：模拟一次系统键（Alt down/up），让系统认为本进程具备前台权
            keybd_event(VK_MENU.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
            keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            ok = SetForegroundWindow(target).as_bool() && GetForegroundWindow() == target;
        }

        if !ok {
            flash(target);
        }
        ok
    }
}

/// 任务栏闪烁：给用户可感知的失败反馈（管理员窗口等 UIPI 受限场景）。
fn flash(hwnd: HWND) {
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: FLASHW_ALL,
        uCount: 4,
        dwTimeout: 0,
    };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}
