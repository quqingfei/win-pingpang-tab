//! 进程名缓存（技术设计 §4.3）：root hwnd → 小写进程名。
//! 钩子回调只做缓存查找（HashMap 纳秒级）；缓存 miss 才走 Win32 查询链。
//! 窗口销毁导致 hwnd 复用错配概率极低，后果仅是多放行一次右键，可接受。

use std::collections::HashMap;
use std::sync::Mutex;

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

static CACHE: Mutex<Option<HashMap<isize, String>>> = Mutex::new(None);

/// 归一到顶层窗口（子窗口句柄 → root）。
pub fn root_of(hwnd: HWND) -> HWND {
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetAncestor(hwnd, windows::Win32::UI::WindowsAndMessaging::GA_ROOT) }
}

/// 查询进程名（小写短名，如 "msedge.exe"）。缓存 miss 时执行 Win32 查询链。
pub fn process_name(root: HWND) -> Option<String> {
    let mut guard = CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(name) = cache.get(&(root.0 as isize)) {
        return Some(name.clone());
    }
    let name = query_process_name(root)?;
    cache.insert(root.0 as isize, name.clone());
    Some(name)
}

fn query_process_name(root: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        // GetWindowThreadProcessId 返回线程 id；pid 由 out 参数获得
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
            root,
            Some(&mut pid),
        );
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(process);
        if !ok {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        let short = full
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&full)
            .to_lowercase();
        Some(short)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cache_returns_lowercase_short_name_shape() {
        // 仅验证归一逻辑本身（字符串处理），Win32 查询依赖真实窗口，不在单测覆盖
        let full = r"C:\Program Files\Some App\Discord.EXE";
        let short: String = full
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(full)
            .to_lowercase();
        assert_eq!(short, "discord.exe");
    }
}
