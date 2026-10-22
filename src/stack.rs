//! 窗口栈构建（技术设计 §3）：实时枚举命中鼠标点的窗口（按 Z 序从顶到底），
//! 目标 = 可见顶层之下的第一层（stack[1]）。无状态、无记录、永远准确。

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId,
};

/// 窗口候选的属性集合——把 Win32 查询与过滤决策分离，谓词可单测。
#[derive(Debug, Clone)]
pub struct Candidate {
    pub hwnd: isize,
    pub rect: RECT,
    pub is_visible: bool,
    pub is_minimized: bool,
    pub is_cloaked: bool,
    pub class_skip: bool,
    pub own_process: bool,
}

/// 过滤谓词（纯函数）：可见、未最小化、非 cloaked、非系统类、非自身进程。
pub fn is_eligible(c: &Candidate) -> bool {
    c.is_visible
        && !c.is_minimized
        && !c.is_cloaked
        && !c.class_skip
        && !c.own_process
}

/// 点在矩形内（纯函数，含边界）。
pub fn point_in_rect(pt: POINT, r: RECT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

/// 目标选择（纯函数）：跳过最顶层（被穿透的可见层），从第二层起取
/// 第一个「不是当前前台窗口」的层。前台层被跳过——激活已是前台的窗口
/// 没有任何可见变化（M1 实测：钉钉置顶迷你窗下 stack[1] 常为前台窗口）。
pub fn pick_target(stack: &[isize], fg_root: isize) -> Option<isize> {
    stack.iter().skip(1).copied().find(|&h| h != fg_root)
}

const SYSTEM_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

struct EnumCtx {
    pt: POINT,
    out: Vec<isize>,
}

/// 构建 pt 处的窗口栈（Win32 部分）：EnumWindows 天然按 Z 序从顶到底回调。
pub fn build_stack(pt: POINT) -> Vec<isize> {
    let mut ctx = EnumCtx { pt, out: Vec::new() };
    let ctx_ptr = &mut ctx as *mut EnumCtx as isize;
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(ctx_ptr));
    }
    ctx.out
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumCtx);
    let c = describe(hwnd);
    if is_eligible(&c) && point_in_rect(ctx.pt, c.rect) {
        ctx.out.push(c.hwnd);
    }
    BOOL(1)
}

/// 汇总单个窗口的属性供谓词判定。
fn describe(hwnd: HWND) -> Candidate {
    unsafe {
        let is_visible = windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool();
        let is_minimized = windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd).as_bool();
        let is_cloaked = is_cloaked_uwp(hwnd);
        let rect = visible_bounds(hwnd).unwrap_or_default_rect();
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Candidate {
            hwnd: hwnd.0 as isize,
            rect,
            is_visible,
            is_minimized,
            is_cloaked,
            class_skip: is_system_class(hwnd),
            own_process: pid != 0 && pid == GetCurrentProcessId(),
        }
    }
}

/// UWP 挂起窗口的 cloaked 状态（技术设计 §3）。
fn is_cloaked_uwp(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let ok = unsafe {
        windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
    };
    ok && cloaked != 0
}

/// 可见边界：优先 DWM 扩展帧（排除阴影），失败退 GetWindowRect。
fn visible_bounds(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    let ok = unsafe {
        windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok()
    };
    if ok && (rect.right > rect.left) {
        return Some(rect);
    }
    let mut fallback = RECT::default();
    let ok2 = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut fallback).is_ok()
    };
    if ok2 && fallback.right > fallback.left {
        Some(fallback)
    } else {
        None
    }
}

fn is_system_class(hwnd: HWND) -> bool {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return false;
    }
    let class = String::from_utf16_lossy(&buf[..n as usize]);
    SYSTEM_CLASSES.contains(&class.as_str())
}

/// RECT 便利 trait：GetWindowRect 失败时退化为"永不命中"矩形。
trait DefaultRect {
    fn unwrap_or_default_rect(self) -> RECT;
}
impl DefaultRect for Option<RECT> {
    fn unwrap_or_default_rect(self) -> RECT {
        self.unwrap_or(RECT {
            left: i32::MAX,
            top: i32::MAX,
            right: i32::MIN,
            bottom: i32::MIN,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(visible: bool, min: bool, cloaked: bool, class_skip: bool, own: bool) -> Candidate {
        Candidate {
            hwnd: 1,
            rect: RECT {
                left: 0,
                top: 0,
                right: 100,
                bottom: 100,
            },
            is_visible: visible,
            is_minimized: min,
            is_cloaked: cloaked,
            class_skip,
            own_process: own,
        }
    }

    #[test]
    fn eligible_rules() {
        assert!(is_eligible(&cand(true, false, false, false, false)));
        assert!(!is_eligible(&cand(false, false, false, false, false)), "不可见");
        assert!(!is_eligible(&cand(true, true, false, false, false)), "最小化");
        assert!(!is_eligible(&cand(true, false, true, false, false)), "cloaked");
        assert!(!is_eligible(&cand(true, false, false, true, false)), "系统类");
        assert!(!is_eligible(&cand(true, false, false, false, true)), "自身进程");
    }

    #[test]
    fn hit_test_geometry() {
        let r = RECT { left: 10, top: 10, right: 110, bottom: 110 };
        assert!(point_in_rect(POINT { x: 50, y: 50 }, r), "内部");
        assert!(point_in_rect(POINT { x: 10, y: 10 }, r), "左上边界含");
        assert!(!point_in_rect(POINT { x: 110, y: 50 }, r), "右边界不含");
        assert!(!point_in_rect(POINT { x: 9, y: 50 }, r), "左侧外部");
        assert!(!point_in_rect(POINT { x: 50, y: 200 }, r), "下方外部");
    }

    #[test]
    fn pick_target_rules() {
        assert_eq!(pick_target(&[], 1), None, "无窗口");
        assert_eq!(pick_target(&[10], 1), None, "仅一层");
        assert_eq!(pick_target(&[10, 20], 10), Some(20), "常规：前台在顶，取第二层");
        assert_eq!(
            pick_target(&[10, 20, 30], 20),
            Some(30),
            "第二层为前台则跳过取更深层"
        );
        assert_eq!(
            pick_target(&[10, 20], 20),
            None,
            "顶层下仅前台层（置顶悬浮窗压着前台）→ 无目标"
        );
        assert_eq!(pick_target(&[10, 20, 30], 99), Some(20), "前台不在栈中时取第二层");
    }
}
