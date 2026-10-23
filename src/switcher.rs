//! 工作线程（技术设计 §1.1/§3/§4.1）：接收激活请求，构建窗口栈，选目标并激活。
//! 维护「翻出状态」：同位置逐层下挖；无目标时收起全部；
//! 跨位置一次触发 = 收起旧位置翻出窗口 + 翻出新目标。

use std::sync::mpsc::Receiver;

use tracing::{debug, info, warn};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYBD_EVENT_FLAGS, VK_MENU, KEYEVENTF_KEYUP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, FlashWindowEx, GetForegroundWindow, GetWindow, GetWindowThreadProcessId,
    IsIconic, IsWindow, SetForegroundWindow, SetWindowPos, FLASHW_ALL, FLASHWINFO, GW_HWNDPREV,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
};

use crate::proc_cache;
use crate::stack::{self, Raised};

/// 钩子线程 → 工作线程的激活请求：使用按下瞬间的 pt（不用事后 GetCursorPos，防打偏）。
pub struct ActivateRequest {
    pub pt: POINT,
}

/// 工作线程私有状态（单线程访问，无需同步）：翻出记录，按翻出顺序排列。
struct SwitchState {
    raised: Vec<Raised>,
}

/// 工作线程主循环。
pub fn run(receiver: Receiver<ActivateRequest>) {
    info!("工作线程已启动");
    let mut state = SwitchState { raised: Vec::new() };
    for req in receiver {
        state.handle(req);
    }
    info!("工作线程退出（channel 已关闭）");
}

impl SwitchState {
    fn handle(&mut self, req: ActivateRequest) {
        let fg = unsafe { GetForegroundWindow() };
        let fg_root = proc_cache::root_of(fg);
        let hit = stack::build_stack(req.pt);
        debug!("pt=({}, {}) 窗口栈(顶→底): {:?}", req.pt.x, req.pt.y, hit);

        // 1. 剪枝：清理已销毁/最小化/被用户手动盖回的翻出记录
        self.prune();

        // 2. 选目标（§3：跳过前台、已翻出层、同链路盖层）
        let Some(target_raw) = stack::pick_target(&hit, fg_root.0 as isize, &self.raised) else {
            if self.raised.is_empty() {
                info!(
                    "未切换: pt=({},{}) | 前台={} | 顶下无其他可翻窗口 | 栈(顶→底)=[{}]",
                    req.pt.x,
                    req.pt.y,
                    layer_name(fg_root),
                    layer_names(&hit)
                );
            } else {
                // 无可翻目标但有翻出窗口 → 全部收起（显示/隐藏乒乓）
                self.restore_all(req.pt);
            }
            return;
        };
        let target = proc_cache::root_of(HWND(target_raw as _));
        let target_name = layer_name(target);
        let fg_name = layer_name(fg_root);

        // 3. 跨位置：收起不在当前栈中的旧翻出窗口（一次触发 = 收旧 + 翻新）
        let (keep, to_restore) = stack::split_by_stack(&self.raised, &hit);
        let mut restored_names = Vec::new();
        for r in to_restore {
            if restore_below(r.target, r.cover) {
                restored_names.push(layer_name(proc_cache::root_of(HWND(r.target as _))));
            } else {
                warn!("收起失败: hwnd=0x{:X}（放回盖层之下）", r.target);
            }
        }
        self.raised = keep.into_iter().copied().collect();
        let restore_note = if restored_names.is_empty() {
            String::new()
        } else {
            format!(" | 同时收起: [{}]", restored_names.join(", "))
        };

        // 4. 激活新目标
        let ok = activate(target);
        if ok {
            // 记录翻出：cover = 本次被穿透的栈顶盖层（收起时放回其下，还原原 Z 序）
            self.raised
                .push(Raised { target: target_raw, cover: hit[0] });
            info!(
                "切换成功: pt=({},{}) | {fg_name} → {target_name} (hwnd=0x{target_raw:X}){restore_note}",
                req.pt.x, req.pt.y
            );
        } else {
            warn!(
                "切换失败(已闪烁): pt=({},{}) | {fg_name} → {target_name} (hwnd=0x{target_raw:X}){restore_note}",
                req.pt.x, req.pt.y
            );
        }
    }

    /// 剪枝：target/cover 已销毁、target 已被用户最小化或手动盖回（Z 序低于其盖层）
    /// 的记录清除——这些窗口已回到"未被工具翻出"的状态，后续触发重新翻出。
    fn prune(&mut self) {
        let before = self.raised.len();
        self.raised.retain(|r| {
            let (t, c) = (HWND(r.target as _), HWND(r.cover as _));
            unsafe {
                IsWindow(Some(t)).as_bool()
                    && IsWindow(Some(c)).as_bool()
                    && !IsIconic(t).as_bool()
                    && is_above(t, c)
            }
        });
        if self.raised.len() != before {
            debug!("翻出记录剪枝: {} → {} 条", before, self.raised.len());
        }
    }

    /// 收起全部翻出窗口：按翻出顺序放回各自盖层之下（还原原 Z 序）。
    fn restore_all(&mut self, pt: POINT) {
        let names: Vec<String> = self
            .raised
            .iter()
            .map(|r| layer_name(proc_cache::root_of(HWND(r.target as _))))
            .collect();
        for r in &self.raised {
            if !restore_below(r.target, r.cover) {
                warn!("收起失败: hwnd=0x{:X}（放回盖层之下）", r.target);
            }
        }
        info!("已收起: [{}] | pt=({},{})", names.join(", "), pt.x, pt.y);
        self.raised.clear();
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

/// target 是否仍位于 cover 之上（Z 序为全局序）：从 target 向上遍历，
/// 遇到 cover 即说明已被用户盖回；到栈顶仍未遇到则仍在 cover 之上。
fn is_above(target: HWND, cover: HWND) -> bool {
    let mut cur = target;
    loop {
        cur = match unsafe { GetWindow(cur, GW_HWNDPREV) } {
            Ok(next) => next,
            Err(_) => return true, // 到栈顶仍未遇到 cover → target 仍在 cover 之上
        };
        if cur == cover {
            return false; // cover 在 target 之上 → 已被盖回
        }
    }
}

/// 收起单个窗口：把 target 放回 cover 之下（Z 序），不移动、不改尺寸。
/// 若 target 是当前前台（收起后会被埋在 cover 之下），把前台转交给 cover——
/// 否则前台句柄指向被埋窗口，下一轮「跳过前台层」规则会误跳它，
/// 两个全屏窗口对切时第三按起永久卡死（2026-10-23 实测）。
fn restore_below(target: isize, cover: isize) -> bool {
    unsafe {
        let ok = SetWindowPos(
            HWND(target as _),
            Some(HWND(cover as _)),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER,
        )
        .is_ok();
        if !ok {
            return false;
        }
        if GetForegroundWindow() == HWND(target as _) {
            activate(HWND(cover as _));
        }
        true
    }
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
