//! 通用工具：DPI 声明、单实例互斥、权限自检（技术设计 §4.2/§4.4/§4.8）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tracing::{error, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// 单实例命名互斥体；进程存活期间必须持有（保活，字段本身不读——持有即锁定）。
pub struct SingleInstance {
    handle: Mutex<HANDLE>,
    acquired: AtomicBool,
}

// HANDLE 是裸指针封装，仅在本进程内互斥使用，不跨线程共享语义问题。
unsafe impl Send for SingleInstance {}
unsafe impl Sync for SingleInstance {}

impl SingleInstance {
    /// 尝试成为唯一实例。返回 Err(()) 表示已有实例在运行。
    pub fn acquire() -> Result<Self, ()> {
        let name: Vec<u16> = "Local\\WinPingPangTab.SingleInstance"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            // 持有句柄即锁定，无需 WaitForSingleObject
            match CreateMutexW(None, false, PCWSTR(name.as_ptr())) {
                Ok(handle) => {
                    // CreateMutexW 成功但互斥体已存在 → 已有实例
                    let already_exists =
                        windows::Win32::Foundation::GetLastError() == ERROR_ALREADY_EXISTS;
                    if already_exists {
                        let _ = CloseHandle(handle);
                        Err(())
                    } else {
                        Ok(Self {
                            handle: Mutex::new(handle),
                            acquired: AtomicBool::new(true),
                        })
                    }
                }
                Err(e) => {
                    error!("创建单实例互斥体失败: {e}");
                    Err(())
                }
            }
        }
    }

    /// 显式释放（自提升交接用，M2；M1 随进程结束自动释放）。
    #[allow(dead_code)]
    pub fn release(&self) {
        if self.acquired.swap(false, Ordering::SeqCst) {
            if let Ok(h) = self.handle.lock() {
                unsafe {
                    let _ = CloseHandle(*h);
                }
            }
        }
    }
}

/// 进程启动第一行调用：声明 Per-Monitor DPI Aware v2（技术设计 §4.2）。
/// 声明后钩子 pt 与窗口矩形同为物理像素，混合 DPI 多屏坐标一致。
/// 返回是否成功（调用方在日志就绪后记录结果）。
pub fn set_dpi_awareness() -> bool {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_ok() }
}

/// 权限自检（技术设计 §4.8）：TokenElevation 判断是否以管理员运行。
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if let Err(e) = OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) {
            warn!("OpenProcessToken 失败: {e}");
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// %APPDATA%\win-pingpang-tab：配置文件 / 自定义图标等用户级文件的根目录（§4.7）。
pub fn app_dir() -> std::path::PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&appdata).join("win-pingpang-tab")
}
