//! 窗口栈构建（技术设计 §3）：实时枚举命中鼠标点的窗口（按 Z 序从顶到底）。
//! 目标选择结合工作线程的「翻出状态」：跳过已翻出层与同链路盖层，逐层下挖。

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, WindowFromPoint,
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

/// 工作线程的翻出记录：target = 已翻出窗口，cover = 翻出时被穿透的栈顶盖层。
/// 收起 = 把 target 放回 cover 之下（还原原 Z 序）。
#[derive(Debug, Clone, Copy)]
pub struct Raised {
    pub target: isize,
    pub cover: isize,
}

/// 目标选择（纯函数）：跳过最顶层（被穿透的盖层），从第二层起取第一个
/// 满足以下全部条件的层：
/// - 非当前前台窗口：激活已是前台的窗口无可见变化（M1 实测：钉钉置顶
///   迷你窗下 stack[1] 常为前台窗口）；
/// - 非已翻出层：窗口已在最前，重复激活无意义，应继续下挖更深层；
/// - 非同链路盖层：盖层的翻出窗口仍在当前栈中时，重新翻盖层 = 把已翻出
///   窗口盖回（乒乓回退）。翻出窗口不在当前栈（跨位置）时盖层可正常作为目标。
pub fn pick_target(stack: &[isize], fg_root: isize, raised: &[Raised]) -> Option<isize> {
    let is_out = |h: isize| raised.iter().any(|r| r.target == h);
    let is_live_cover = |h: isize| {
        raised
            .iter()
            .any(|r| r.cover == h && stack.contains(&r.target))
    };
    stack.iter().skip(1).copied().find(|&h| {
        h != fg_root && !is_out(h) && !is_live_cover(h)
    })
}

/// 翻出记录按当前位置拆分（纯函数）：target 出现在当前栈中的记录保留
/// （同链路逐层下挖，已翻出层保持显示）；不在栈中的记录待收起
/// （跨位置切换：一次触发 = 收起旧位置翻出窗口 + 翻出新目标）。
/// 返回 (保留, 待收起)，均保持原翻出顺序（收起按翻出顺序还原原 Z 序）。
pub fn split_by_stack<'a>(raised: &'a [Raised], stack: &[isize]) -> (Vec<&'a Raised>, Vec<&'a Raised>) {
    raised.iter().partition(|r| stack.contains(&r.target))
}

const SYSTEM_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

struct EnumCtx {
    pt: POINT,
    out: Vec<isize>,
}

/// 构建 pt 处的窗口栈（Win32 部分）：EnumWindows 天然按 Z 序从顶到底回调。
/// 截断规则：以 `WindowFromPoint`（鼠标事件实际命中的窗口，系统自动跳过
/// 点击穿透的透明层）为锚，栈中位于锚之上的层全部丢弃——鼠标特效软件
/// （如 mfcmouseeffect.exe）的置顶透明全屏窗口会通过常规过滤混入每个点
/// 的栈顶（实测：污染 cover 记录 + 触发剪枝误判"已被盖回"），锚定后
/// 栈顶恒为用户实际看到/点到的窗口。锚不在栈中（被过滤，如任务栏）时
/// 保留全栈。
pub fn build_stack(pt: POINT) -> Vec<isize> {
    let mut ctx = EnumCtx { pt, out: Vec::new() };
    let ctx_ptr = &mut ctx as *mut EnumCtx as isize;
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(ctx_ptr));
    }
    let hit_root = unsafe { WindowFromPoint(pt) };
    if hit_root.0.is_null() {
        return ctx.out;
    }
    let anchor = crate::proc_cache::root_of(hit_root).0 as isize;
    match ctx.out.iter().position(|&h| h == anchor) {
        Some(i) => ctx.out.split_off(i),
        None => ctx.out,
    }
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
        assert_eq!(pick_target(&[], 1, &[]), None, "无窗口");
        assert_eq!(pick_target(&[10], 1, &[]), None, "仅一层");
        assert_eq!(pick_target(&[10, 20], 10, &[]), Some(20), "常规：前台在顶，取第二层");
        assert_eq!(
            pick_target(&[10, 20, 30], 20, &[]),
            Some(30),
            "第二层为前台则跳过取更深层"
        );
        assert_eq!(
            pick_target(&[10, 20], 20, &[]),
            None,
            "顶层下仅前台层（置顶悬浮窗压着前台）→ 无目标"
        );
        assert_eq!(pick_target(&[10, 20, 30], 99, &[]), Some(20), "前台不在栈中时取第二层");
    }

    #[test]
    fn pick_target_dig_deeper() {
        // 栈 [X1(已翻出), T(其盖层), X2]：X1 已在最前、重翻 T 会把 X1 盖回 → 取 X2
        let raised = [Raised { target: 11, cover: 10 }];
        assert_eq!(pick_target(&[11, 10, 12], 11, &raised), Some(12), "逐层下挖");
    }

    #[test]
    fn pick_target_chain_exhausted() {
        // 栈 [X2, X1, T]：全部为已翻出层/同链路盖层 → 无目标（触发收起）
        let raised = [
            Raised { target: 11, cover: 10 },
            Raised { target: 12, cover: 11 },
        ];
        assert_eq!(pick_target(&[12, 11, 10], 12, &raised), None, "挖尽 → 收起");
    }

    #[test]
    fn pick_target_cover_of_absent_out_window() {
        // 翻出窗口 X1 不在当前栈（跨位置）：其盖层 T 可作为新目标正常翻出
        let raised = [Raised { target: 11, cover: 10 }];
        assert_eq!(
            pick_target(&[20, 10, 21], 20, &raised),
            Some(10),
            "跨位置：盖层本身可被翻出"
        );
        // 对照：X1 仍在当前栈中时 T 被跳过（防乒乓回退）
        assert_eq!(
            pick_target(&[20, 11, 10, 21], 20, &raised),
            Some(21),
            "同链路：跳过盖层取更深层"
        );
    }

    #[test]
    fn split_by_stack_rules() {
        let raised = [
            Raised { target: 11, cover: 10 }, // X1 在栈中 → 保留（同链路下挖）
            Raised { target: 21, cover: 20 }, // Y1 不在栈中 → 待收起（跨位置）
        ];
        let (keep, restore) = split_by_stack(&raised, &[11, 10]);
        assert_eq!(keep.len(), 1, "栈中保留 1 条");
        assert_eq!(keep[0].target, 11);
        assert_eq!(restore.len(), 1, "栈外收起 1 条");
        assert_eq!(restore[0].target, 21);

        // 当前栈与所有翻出窗口无关 → 全部收起（无目标收起分支）
        let (keep2, restore2) = split_by_stack(&raised, &[30, 31]);
        assert!(keep2.is_empty());
        assert_eq!(restore2.len(), 2);

        // 顺序保持：收起按翻出顺序
        let (_, restore3) = split_by_stack(&raised, &[31, 30]);
        assert_eq!(restore3[0].target, 11);
        assert_eq!(restore3[1].target, 21);
    }
}
