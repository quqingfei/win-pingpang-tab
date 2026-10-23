//! 开机自启动（技术设计 §4.5）：HKCU Run 勾选式，托盘菜单触发。
//! HKCU Run 无需管理员权限；代价是要求管理员时无法绕过 UAC（见 §4.8，V1 不处理）。

use std::io;

use windows_registry::CURRENT_USER;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "WinPingPangTab";

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// 当前是否已开启自启动（值不存在/为空视为关闭）。
pub fn is_enabled() -> bool {
    CURRENT_USER
        .open(RUN_KEY)
        .ok()
        .and_then(|key| key.get_string(VALUE_NAME).ok())
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// 开启/关闭自启动（失败返回错误由调用方记日志）。
pub fn set_enabled(on: bool) -> io::Result<()> {
    let key = CURRENT_USER.create(RUN_KEY).map_err(io_err)?;
    if on {
        let exe = std::env::current_exe()?;
        // 引号包裹：防 exe 路径含空格被 Run 解析截断
        key.set_string(VALUE_NAME, format!("\"{}\"", exe.display()))
            .map_err(io_err)?;
    } else {
        // 值不存在时报错——toggle 路径仅在已开启时走到关闭分支，正常不会命中
        key.remove_value(VALUE_NAME).map_err(io_err)?;
    }
    Ok(())
}
