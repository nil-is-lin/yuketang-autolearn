// 把图标和版本信息写进 exe 的资源段：任务栏、桌面快捷方式、文件属性里都能看到。
// 资源编译要 rc.exe，找不到时只警告、不中断构建（没有图标也能跑）。
fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "雨课堂自动学习");
        res.set("FileDescription", "雨课堂自动学习（登录后自动刷课）");
        res.set("CompanyName", "yuketang-autolearn");
        res.set("OriginalFilename", "yuketang-autolearn.exe");
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        if let Err(e) = res.compile() {
            println!("cargo:warning=图标资源没能嵌入（{e}），exe 仍可运行");
        }
    }
}
