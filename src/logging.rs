//! 日志（技术设计 §7）：tracing + 单文件 app.log，超过 max_size（默认 10MB）自动清空重写。
//! 铁律：钩子回调内禁止任何 IO（含日志）——回调留痕经 channel 交工作线程落盘。

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::info;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;

struct Inner {
    file: File,
    written: u64,
    max_bytes: u64,
}

/// 大小受限的同步 writer：写前检查，超限即截断（set_len(0)）从头继续写。
/// 注意：不能用 append 模式打开——Windows 上 append 句柄（FILE_APPEND_DATA）
/// 调 set_len 会报 ERROR_ACCESS_DENIED，故普通 write 句柄 + 手动 seek。
#[derive(Clone)]
pub struct SizeLimitedWriter {
    inner: Arc<Mutex<Inner>>,
}

impl SizeLimitedWriter {
    pub fn new(path: &Path, max_bytes: u64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new().write(true).create(true).open(path)?;
        let existing = file.seek(SeekFrom::End(0))?; // 续写：定位到现有文件末尾
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                file,
                written: existing,
                max_bytes,
            })),
        })
    }
}

impl Write for SizeLimitedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        if inner.written >= inner.max_bytes {
            // 截断清空，从头重写；写位置手动复位，与 written 计数保持同步
            inner.file.set_len(0)?;
            inner.file.seek(SeekFrom::Start(0))?;
            inner.written = 0;
        }
        let n = inner.file.write(buf)?;
        inner.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.lock().unwrap().file.flush()
    }
}

impl MakeWriter<'_> for SizeLimitedWriter {
    type Writer = SizeLimitedWriter;
    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

/// 日志目录：%APPDATA%\win-pingpang-tab\logs\
pub fn log_dir() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    Path::new(&appdata).join("win-pingpang-tab").join("logs")
}

/// 初始化全局日志（级别/上限来自配置文件，§5/§7；调用方已保证值合法）。
/// 幂等：重复调用不会覆盖已安装的 subscriber。
pub fn init(level: &str, max_bytes: u64) {
    let path = log_dir().join("app.log");
    let writer = match SizeLimitedWriter::new(&path, max_bytes) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("日志文件创建失败（日志功能将被禁用）: {e}");
            return;
        }
    };
    // 级别字符串已在 config::load 校验；此处兜底未知值 → INFO
    let lf = match level.to_ascii_lowercase().as_str() {
        "off" => LevelFilter::OFF,
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        _ => LevelFilter::INFO,
    };
    let installed = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .with_max_level(lf)
        .try_init()
        .is_ok();
    if installed {
        info!(
            "日志已启用: {} (level={level}, 上限 {} MB, 超限清空重写)",
            path.display(),
            max_bytes / 1024 / 1024
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_when_exceeds_max() {
        let dir = std::env::temp_dir().join(format!("ftc-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.log");
        let mut w = SizeLimitedWriter::new(&path, 64).unwrap();

        // 写前检查语义：已写 96 ≥ 64 时第三条写入前截断（允许文件短暂超限至 max+单条，
        // 换取最后一条日志永远完整）
        let line = vec![b'x'; 48];
        w.write_all(&line).unwrap();
        w.write_all(&line).unwrap();
        w.write_all(&line).unwrap(); // 此条写入前触发截断 → 文件=48B（仅此条）
        w.flush().unwrap();

        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(len, 48, "截断后应只剩最后一条完整记录");
        // 继续写仍正常
        w.write_all(&line).unwrap();
        w.flush().unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(len, 96);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
