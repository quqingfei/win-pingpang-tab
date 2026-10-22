//! 配置文件（技术设计 §5）：%APPDATA%\fast-tab-click\config.toml。
//! 四原则：单一来源 / 文件即文档 / 容错不拒启 / 重启生效。
//! 落地状态（2026-10-22）：[trigger]/[blacklist]/[log] 已接入；
//! [privilege] 随自提升功能接入（模板暂未包含，防误配）。

use std::path::PathBuf;

use crate::util;

/// 运行配置（已归一化：小写、去空白；非法字段在加载时回落默认并收集消息）。
#[derive(Debug, Clone)]
pub struct Config {
    pub modifiers: Vec<String>,
    pub button: String,
    pub blacklist: Vec<String>,
    pub log_level: String,
    pub log_max_mb: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            modifiers: vec!["lalt".into()],
            button: "right".into(),
            blacklist: vec!["mstsc.exe".into()],
            log_level: "info".into(),
            log_max_mb: 10,
        }
    }
}

pub fn config_path() -> PathBuf {
    util::app_dir().join("config.toml")
}

/// 默认模板（文件即文档：带中文注释，首次运行生成）。
pub const DEFAULT_TEMPLATE: &str = r#"# ==============================================================
#  FastTabClick 配置文件
#  修改保存后重启程序生效：托盘右键 → 退出 → 重新启动
# ==============================================================

[trigger]
# 触发组合键的修饰键部分：触发时以下键须全部按住（至少 1 个，可多选）
# 可选值：lalt  ralt  lctrl  rctrl  lshift  rshift  lwin  rwin
modifiers = ["lalt"]

# 触发鼠标键（单选）
# 可选值：right（右键）  middle（中键）  x1（侧键1）  x2（侧键2）
button = "right"

[blacklist]
# 进程黑名单：鼠标悬停的顶层窗口属于这些进程时，触发键原样放行
# （如远程桌面 mstsc.exe 自带 Alt+右键功能，建议保留）
processes = ["mstsc.exe"]

[log]
# 日志级别：off | error | info | debug
level = "info"

# 日志单文件上限（MB）：超过后自动清空重写
max_size_mb = 10
"#;

/// 确保默认配置存在（首次运行生成；托盘「配置文件」也调用）。
/// 返回 (路径, 是否本次生成)。
pub fn ensure_default() -> (PathBuf, bool) {
    let path = config_path();
    if path.exists() {
        return (path, false);
    }
    let _ = std::fs::create_dir_all(util::app_dir());
    let created = std::fs::write(&path, DEFAULT_TEMPLATE).is_ok();
    (path, created)
}

/// 加载配置：文件缺失/解析失败/字段非法 → 回落默认值，不阻断启动。
/// 返回 (Config, 消息列表)；消息由调用方在日志就绪后统一 warn 记录
/// （配置先于日志加载，此处不能直接用 tracing——无订阅者会丢）。
pub fn load() -> (Config, Vec<String>) {
    let mut msgs = Vec::new();
    let (path, created) = ensure_default();
    if created {
        msgs.push(format!("首次运行：已生成默认配置文件 {}", path.display()));
    }

    let mut cfg = Config::default();
    let Ok(content) = std::fs::read_to_string(&path) else {
        msgs.push(format!("配置文件读取失败，全部使用默认值: {}", path.display()));
        return (cfg, msgs);
    };
    let table = match toml::from_str::<toml::Table>(&content) {
        Ok(t) => t,
        Err(e) => {
            msgs.push(format!("配置文件解析失败，全部使用默认值: {e}"));
            return (cfg, msgs);
        }
    };
    apply_table(&mut cfg, &mut msgs, &table);
    (cfg, msgs)
}

/// 字段提取/校验/回落（load 与测试共用同一逻辑，防模板与解析脱节）。
fn apply_table(cfg: &mut Config, msgs: &mut Vec<String>, table: &toml::Table) {
    // [trigger]
    if let Some(t) = table.get("trigger") {
        if let Some(arr) = t.get("modifiers").and_then(|v| v.as_array()) {
            let mut mods = Vec::new();
            for v in arr {
                match v.as_str() {
                    Some(s) if !s.trim().is_empty() => mods.push(s.trim().to_ascii_lowercase()),
                    _ => msgs.push("trigger.modifiers 含非字符串或空项，已忽略该项".into()),
                }
            }
            if mods.is_empty() {
                msgs.push("trigger.modifiers 为空，回落默认 [\"lalt\"]".into());
            } else {
                cfg.modifiers = mods;
            }
        }
        if let Some(b) = t.get("button").and_then(|v| v.as_str()) {
            let b = b.trim().to_ascii_lowercase();
            if matches!(b.as_str(), "right" | "middle" | "x1" | "x2") {
                cfg.button = b;
            } else {
                msgs.push(format!(
                    "trigger.button \"{b}\" 无法识别（可选 right/middle/x1/x2），回落 right"
                ));
            }
        }
    }

    // [blacklist]
    if let Some(b) = table.get("blacklist") {
        if let Some(arr) = b.get("processes").and_then(|v| v.as_array()) {
            let mut list = Vec::new();
            for v in arr {
                match v.as_str() {
                    Some(s) if !s.trim().is_empty() => list.push(s.trim().to_ascii_lowercase()),
                    _ => msgs.push("blacklist.processes 含非字符串或空项，已忽略该项".into()),
                }
            }
            cfg.blacklist = list;
        }
    }

    // [log]
    if let Some(l) = table.get("log") {
        if let Some(s) = l.get("level").and_then(|v| v.as_str()) {
            let s = s.trim().to_ascii_lowercase();
            if matches!(s.as_str(), "off" | "error" | "warn" | "info" | "debug" | "trace") {
                cfg.log_level = s;
            } else {
                msgs.push(format!(
                    "log.level \"{s}\" 无法识别（off/error/info/debug），回落 info"
                ));
            }
        }
        if let Some(n) = l.get("max_size_mb").and_then(|v| v.as_integer()) {
            if (1..=1024).contains(&n) {
                cfg.log_max_mb = n as u64;
            } else {
                msgs.push(format!("log.max_size_mb {n} 越界（1~1024），回落 10"));
            }
        }
    }

    // [privilege]：预留字段，自提升功能接入前不生效
    if let Some(p) = table.get("privilege") {
        if p.get("require_admin").and_then(|v| v.as_bool()) == Some(true) {
            msgs.push(
                "[privilege] require_admin=true：该功能尚未接入（后续版本生效），当前按现有权限运行"
                    .into(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试辅助：直接解析文本（不落盘），与 load 共用 apply_table 回落逻辑。
    fn parse_for_test(content: &str) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        let mut msgs = Vec::new();
        match toml::from_str::<toml::Table>(content) {
            Ok(table) => apply_table(&mut cfg, &mut msgs, &table),
            Err(e) => msgs.push(format!("配置文件解析失败，全部使用默认值: {e}")),
        }
        (cfg, msgs)
    }

    #[test]
    fn default_template_is_valid_toml() {
        // 模板必须能被解析回默认值（文件即文档，防模板与解析逻辑脱节）
        let (cfg, msgs) = parse_for_test(DEFAULT_TEMPLATE);
        assert_eq!(cfg.modifiers, vec!["lalt".to_string()]);
        assert_eq!(cfg.button, "right");
        assert_eq!(cfg.blacklist, vec!["mstsc.exe".to_string()]);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.log_max_mb, 10);
        assert!(msgs.is_empty(), "默认模板不应产生告警: {msgs:?}");
    }

    #[test]
    fn invalid_fields_fall_back() {
        let bad = r#"
[trigger]
modifiers = []
button = "left"
[blacklist]
processes = [123, "ok.exe"]
[log]
level = "verbose"
max_size_mb = 0
"#;
        let (cfg, msgs) = parse_for_test(bad);
        assert_eq!(cfg.modifiers, vec!["lalt".to_string()]);
        assert_eq!(cfg.button, "right");
        assert_eq!(cfg.blacklist, vec!["ok.exe".to_string()]);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.log_max_mb, 10);
        assert_eq!(msgs.len(), 5);
    }

    #[test]
    fn parse_garbage_uses_defaults() {
        let (cfg, msgs) = parse_for_test("this is not toml [[[[");
        assert_eq!(cfg.modifiers, vec!["lalt".to_string()]);
        assert_eq!(cfg.log_max_mb, 10);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn normalizes_case_and_space() {
        let t = r#"
[trigger]
modifiers = [" LALT ", "RAlt"]
button = "X1"
[blacklist]
processes = ["MSTSC.EXE"]
[log]
level = "DEBUG"
"#;
        let (cfg, msgs) = parse_for_test(t);
        assert_eq!(
            cfg.modifiers,
            vec!["lalt".to_string(), "ralt".to_string()]
        );
        assert_eq!(cfg.button, "x1");
        assert_eq!(cfg.blacklist, vec!["mstsc.exe".to_string()]);
        assert_eq!(cfg.log_level, "debug");
        assert!(msgs.is_empty(), "{msgs:?}");
    }
}
