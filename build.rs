//! build.rs：把 assets/icon.ico 嵌入 exe 资源（Explorer 文件图标）。
//! 尽力而为：资源编译器缺失时仅警告不阻断构建（托盘图标不依赖此项）。

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if let Err(e) = winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
        {
            println!("cargo:warning=exe 图标嵌入失败（不影响托盘图标）: {e}");
        }
    }
}
