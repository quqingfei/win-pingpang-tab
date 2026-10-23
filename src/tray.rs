//! 系统托盘（技术设计 §4.5，退出能力提前到 M1）：
//! 隐藏窗口 + Shell_NotifyIconW + 右键菜单「退出」。
//! 主线程消息循环即托盘循环——退出 → WM_QUIT → main 收尾（通知钩子线程卸载后 join）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use tracing::{error, info};
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CreateDIBSection, DeleteObject, DIB_RGB_COLORS,
    RGBQUAD,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NOTIFYICONDATAW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
    NIM_DELETE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, HICON, ICONINFO,
    IDI_APPLICATION, IMAGE_ICON, LoadIconW, LoadImageW, LR_DEFAULTSIZE, LR_LOADFROMFILE,
    MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, SW_SHOWNORMAL, TPM_RIGHTBUTTON, TrackPopupMenu, WINDOW_EX_STYLE,
    WINDOW_STYLE, WNDCLASSW, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_NULL,
    WM_RBUTTONUP,
};

/// 托盘回调消息：Shell_NotifyIconW 把鼠标事件以该 msg 发到 hWnd，lParam=鼠标消息。
const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
const IDM_EXIT: usize = 1001;
const IDM_CONFIG: usize = 1002;
const IDM_AUTOSTART: usize = 1003;

/// 已加载托盘图标（0 = 系统共享图标，不销毁）；owned 标记需要 DestroyIcon 的自持句柄。
static TRAY_ICON: AtomicIsize = AtomicIsize::new(0);
static TRAY_ICON_OWNED: AtomicBool = AtomicBool::new(false);

/// UTF-16 空终止字符串（0.62 无 w! 宏，本地等价实现）。
fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 托盘主循环：创建隐藏窗口 → 添加托盘图标 → 消息循环（阻塞直至用户选择退出）。
/// tip：悬浮提示文本（main 依配置生成，随触发键变化）。
pub fn run(tip: String) {
    unsafe {
        let hinstance = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(e) => {
                error!("GetModuleHandleW 失败，托盘不可用: {e}");
                return;
            }
        };
        let class_name = wstr("WinPingPangTabTrayWnd");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(tray_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            error!("注册托盘窗口类失败，托盘不可用");
            return;
        }
        // 普通顶层窗口（非 message-only），不 WS_VISIBLE 即隐藏——托盘回调要求
        let window_name = wstr("WinPingPangTab");
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_name.as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                error!("创建托盘窗口失败: {e}");
                return;
            }
        };
        add_icon(hwnd, load_tray_icon(), tip);

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW_noimport(&mut msg, Some(hwnd), 0, 0);
            if r.0 == 0 || r.0 == -1 {
                break;
            }
            let _ = DispatchMessageW(&msg);
        }
        // 兜底清理（正常路径已在 WM_DESTROY 删除图标）
        remove_icon(hwnd);
        destroy_tray_icon();
    }
}

// GetMessageW 需在 WindowsAndMessaging 中 import，此处用别名避免与钩子线程混淆
use windows::Win32::UI::WindowsAndMessaging::GetMessageW as GetMessageW_noimport;

unsafe extern "system" fn tray_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY_CALLBACK => match lparam.0 as u32 {
            WM_RBUTTONUP | WM_CONTEXTMENU => {
                show_menu(hwnd);
                LRESULT(0)
            }
            _ => LRESULT(0), // 左键等事件忽略（M2 可扩展气泡/主菜单）
        },
        WM_COMMAND => {
            match wparam.0 & 0xFFFF {
                IDM_EXIT => {
                    let _ = DestroyWindow(hwnd);
                }
                IDM_CONFIG => open_config(),
                IDM_AUTOSTART => toggle_autostart(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_icon(hwnd);
            destroy_tray_icon();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 右键托盘：弹出菜单【配置文件】【开机自启动(✓)】——【退出】。
/// 经典托盘修复：SetForegroundWindow + 事后 WM_NULL，否则菜单在别处点击时不会自动消失。
unsafe fn show_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else {
        error!("CreatePopupMenu 失败");
        return;
    };
    let items: [(&str, usize, bool); 3] = [
        ("配置文件", IDM_CONFIG, false),
        ("开机自启动", IDM_AUTOSTART, crate::autostart::is_enabled()),
        ("退出 WinPingPangTab", IDM_EXIT, false),
    ];
    for (i, (text, id, checked)) in items.iter().enumerate() {
        if i == 2 {
            // 分隔线隔开「退出」，降低误点
            if let Err(e) = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()) {
                error!("AppendMenuW(分隔线) 失败: {e}");
            }
        }
        let t = wstr(text);
        let mut flags = MF_STRING;
        if *checked {
            flags |= MF_CHECKED;
        }
        if let Err(e) = AppendMenuW(menu, flags, *id, PCWSTR(t.as_ptr())) {
            error!("AppendMenuW({text}) 失败: {e}");
            let _ = DestroyMenu(menu);
            return;
        }
    }
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
}

fn add_icon(hwnd: HWND, icon: HICON, tip: String) {
    let mut nid = NOTIFYICONDATAW::default();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY_CALLBACK;
    nid.hIcon = icon;
    // szTip 容量 128（含空终止），超长截断防越界
    let tip: Vec<u16> = tip.encode_utf16().take(127).collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    if unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool() {
        info!("托盘图标已添加");
    } else {
        error!(
            "添加托盘图标失败: {}",
            std::io::Error::last_os_error()
        );
    }
}

fn remove_icon(hwnd: HWND) {
    let mut nid = NOTIFYICONDATAW::default();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    // 已删除时 NIM_DELETE 报错——忽略，幂等清理
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

/// 托盘菜单「配置文件」：确保默认配置存在（首字段生成）后用系统关联程序打开；
/// 无 .toml 关联程序时用记事本兜底。
fn open_config() {
    let (path, _created) = crate::config::ensure_default();
    let path_str = path.to_string_lossy().to_string();
    let verb = wstr("open");
    let file = wstr(&path_str);
    let r = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if r.0 as isize > 32 {
        return;
    }
    let notepad = wstr("notepad.exe");
    let params = wstr(&path_str);
    let r2 = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(notepad.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if r2.0 as isize <= 32 {
        error!("打开配置文件失败: {}", std::io::Error::last_os_error());
    }
}

/// 托盘菜单「开机自启动」：勾选式开关（HKCU Run，§4.5）。
fn toggle_autostart() {
    let target = !crate::autostart::is_enabled();
    match crate::autostart::set_enabled(target) {
        Ok(()) => info!("开机自启动: {}", if target { "已开启" } else { "已关闭" }),
        Err(e) => error!("设置开机自启动失败: {e}"),
    }
}

/// 托盘图标加载链（§4.5）：
/// exe 目录 icon.ico → 配置目录 icon.ico → 内嵌默认图标 → 系统默认。
/// 用户自定义方式：把任意 .ico 重命名为 icon.ico 放到前两处任一位置，重启生效。
fn load_tray_icon() -> HICON {
    // 1. exe 旁的 icon.ico（便携使用友好）
    if let Some(dir) = exe_dir() {
        if let Some(h) = load_icon_from_file(&dir.join("icon.ico"), "exe 目录 icon.ico") {
            return h;
        }
    }
    // 2. 配置目录 icon.ico（%APPDATA%\win-pingpang-tab\，写权限永远可用）
    if let Some(h) =
        load_icon_from_file(&crate::util::app_dir().join("icon.ico"), "配置目录 icon.ico")
    {
        return h;
    }
    // 3. 内嵌默认图标（assets/icon.png 编译进二进制，无需资源编译器）
    if let Some(h) = embedded_icon() {
        info!("托盘图标来源: 内嵌默认图标");
        store_owned(h);
        return h;
    }
    // 4. 系统默认兜底
    info!("托盘图标来源: 系统默认（内嵌图标解码失败）");
    unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// 从 .ico 文件加载（LoadImageW 要求文件路径以空终止 UTF-16 传入）。
fn load_icon_from_file(path: &Path, label: &str) -> Option<HICON> {
    if !path.is_file() {
        return None;
    }
    let p = wstr(&path.to_string_lossy());
    let handle = unsafe {
        LoadImageW(
            None,
            PCWSTR(p.as_ptr()),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE | LR_DEFAULTSIZE,
        )
        .ok()
    }?;
    let hicon = HICON(handle.0);
    info!("托盘图标来源: {label} ({})", path.display());
    store_owned(hicon);
    Some(hicon)
}

/// 内嵌 PNG → 解码 → HICON。
fn embedded_icon() -> Option<HICON> {
    const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
    let (w, h, rgba) = decode_png_rgba(ICON_PNG)?;
    create_argb_icon(w, h, &rgba)
}

/// 最小 PNG 解码（RGB/RGBA/灰度 → RGBA8）。图标文件由仓库脚本生成，格式可控。
fn decode_png_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(&buf[..(w * h * 4) as usize]),
        png::ColorType::Rgb => {
            for px in buf.chunks_exact(3) {
                rgba.extend_from_slice(px);
                rgba.push(255);
            }
        }
        png::ColorType::Grayscale => {
            for v in buf {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for px in buf.chunks_exact(2) {
                rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
        }
        _ => return None,
    }
    Some((w, h, rgba))
}

/// 32bpp ARGB 像素 → HICON：颜色 DIB（负高=top-down）+ 1bpp 全零掩码 + CreateIconIndirect。
/// 掩码位图 CreateDIBSection 天然清零——32bpp 带 alpha 的图标掩码语义被忽略，仅需句柄有效。
fn create_argb_icon(w: u32, h: u32, rgba: &[u8]) -> Option<HICON> {
    unsafe {
        let color_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default(); 1],
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &color_info, DIB_RGB_COLORS, &mut bits, None, 0)
            .ok()?;
        if bits.is_null() {
            let _ = DeleteObject(color.into());
            return None;
        }
        // RGBA → BGRA（DIB 内存字节序）
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
        for (o, px) in dst.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            o[0] = px[2];
            o[1] = px[1];
            o[2] = px[0];
            o[3] = px[3];
        }

        let mask_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 1,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default(); 1],
        };
        let mut mask_bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let mask = CreateDIBSection(None, &mask_info, DIB_RGB_COLORS, &mut mask_bits, None, 0)
            .ok()?;
        if mask_bits.is_null() {
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
            return None;
        }

        let ii = ICONINFO {
            fIcon: BOOL(1),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let hicon = CreateIconIndirect(&ii).ok();
        // 系统已复制位图数据，两块 DIB 立即可销毁
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        hicon
    }
}

fn store_owned(h: HICON) {
    TRAY_ICON.store(h.0 as isize, Ordering::SeqCst);
    TRAY_ICON_OWNED.store(true, Ordering::SeqCst);
}

/// 释放自持图标句柄（系统共享图标不销毁）。幂等。
fn destroy_tray_icon() {
    if TRAY_ICON_OWNED.swap(false, Ordering::SeqCst) {
        let h = TRAY_ICON.swap(0, Ordering::SeqCst);
        if h != 0 {
            let _ = unsafe { DestroyIcon(HICON(h as _)) };
        }
    }
}
