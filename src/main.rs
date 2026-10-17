// 发布版按 GUI 子系统链接：双击运行不会弹出控制台窗口，日志都进主窗口下半部分。
// 调试版保留控制台，方便命令行看输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adapter;
mod browser;
mod logging;
mod player;
mod probe;
mod state;
mod verify;
mod watch;
mod web;

use adapter::Adapter;
use clap::{Parser, Subcommand};
use state::{Status, Store};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "yuketang-autolearn", version, about = "雨课堂自动学习（内嵌 WebView2，登录一次后自动刷课）")]
struct Cli {
    /// 机器上没有 WebView2 运行时时，指定 Edge 的安装目录
    #[arg(long, global = true)]
    browser: Option<String>,
    /// 独立用户数据目录，登录一次后长期复用
    #[arg(long, global = true, default_value = "profile")]
    profile: PathBuf,
    /// 适配器目录
    #[arg(long, global = true, default_value = "adapters")]
    adapters: PathBuf,
    /// 页面缩放系数：0 表示按窗口宽度自适应（默认），填 0.3~2.0 里的数则固定
    #[arg(long, global = true, default_value_t = 0.0)]
    zoom: f64,
    /// 切换账号：先清空已保存的登录态，启动后重新扫码/输密码登录
    #[arg(long, global = true)]
    switch: bool,
    /// 保留登录态（下次启动免登录）。默认不保留：每次启动都要登录，退出即删会话
    #[arg(long, global = true)]
    keep_login: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// 起点地址的解析顺序：命令行 → 上次跑过的地址 → 环境变量。
/// 刻意不内置任何学校/课程地址：仓库是公共的，把某个学校的内网入口写死在代码里
/// 既没意义也泄露信息。第一次用请自己给 --url，之后就记在 state/last_url.txt 里。
fn resolve_entry(explicit: Option<String>) -> Option<String> {
    if let Some(u) = explicit {
        return Some(u);
    }
    let last = Path::new("state").join("last_url.txt");
    if let Ok(s) = std::fs::read_to_string(&last) {
        let s = s.trim().to_string();
        if s.starts_with("http") {
            return Some(s);
        }
    }
    std::env::var("YKT_ENTRY").ok().filter(|s| s.starts_with("http"))
}

/// 编译进二进制的适配器，缺 adapters 目录时兜底；磁盘上的同名文件优先（方便平台改版时热改）。
const BUILTIN_ADAPTERS: &[(&str, &str)] =
    &[("yuketang.toml", include_str!("../adapters/yuketang.toml"))];

#[derive(Subcommand)]
enum Cmd {
    /// 打开真实页面，按 URL 落结构快照，用来补齐适配器
    Probe {
        #[arg(long)]
        url: String,
        #[arg(long, default_value = "dumps")]
        out: PathBuf,
        #[arg(long, default_value_t = 40)]
        max_pages: usize,
        /// 逗号分隔的选择器（支持 text:去学习|开始学习），给了就自动点，不用人守着
        #[arg(long, value_delimiter = ',')]
        auto_click: Vec<String>,
    },
    /// 自动连播
    Learn {
        /// 培训项目详情页地址；不填就复用上次跑过的地址（state/last_url.txt）
        #[arg(long)]
        url: Option<String>,
        /// 倍速，默认 2.0（安全上限）；再高有被平台清空进度的风险
        #[arg(long, default_value_t = 2.0)]
        rate: f64,
        /// 这次最多刷完几项，0 = 刷到没有下一项为止（默认）
        #[arg(long, default_value_t = 0)]
        max: usize,
    },
    /// 抓一条真实心跳 payload，改步进后在页面里回放，比对进度接口的变化
    Verify {
        #[arg(long)]
        url: String,
        /// 回放时造多少条新样本；首次验证建议 1~3 条，灌太多容易进异常判定
        #[arg(long, default_value_t = 3)]
        samples: usize,
        /// 等心跳出现的秒数
        #[arg(long, default_value_t = 180)]
        wait: u64,
        /// 只打印抓到的 payload，不发回放请求
        #[arg(long)]
        peek: bool,
    },
    /// 打印适配器路由表
    Routes,
    /// 盯着一个页面看：媒体、iframe、控制层是否就位，用来定位"找不到播放器"
    Watch {
        #[arg(long)]
        url: String,
        #[arg(long, default_value_t = 60)]
        seconds: u64,
        #[arg(long, default_value_t = 3)]
        interval: u64,
        /// 逗号分隔的入口选择器；给了就依次点下去，把流程推进到视频页
        #[arg(long, value_delimiter = ',')]
        click: Vec<String>,
    },
    /// 查看断点续刷状态
    Status {
        #[arg(long)]
        url: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    // 双击 exe 时没有任何参数：直接进 learn，跑完停在这里等人看结果，别闪退。
    let bare = std::env::args().count() <= 1;
    // 必须最先接管 stdout：日志面板靠它工作，晚一步就没法拿到前面打的东西。
    // 隐藏控制台的判断在 install 里做（只隐藏自己独占的那个，不动用户的终端）。
    logging::install(true);
    let mut cli = Cli::parse();
    if cli.cmd.is_none() {
        cli.cmd = Some(Cmd::Learn { url: None, rate: 2.0, max: 0 });
    }
    // 默认不留登录态：每次启动都用一个临时会话目录，退出就删掉。
    // 这样换人用、公用电脑都不需要额外操作，也不会把别人的登录态留在磁盘上。
    let ephemeral = !cli.keep_login;
    let profile = if ephemeral {
        // 放工作区里而不是系统临时目录：有些机器 Temp 目录权限收紧，
        // WebView2 建不了用户数据目录会直接报"拒绝访问"。
        Path::new("state").join(format!("session-{}", std::process::id()))
    } else {
        cli.profile.clone()
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = rt.block_on(run(cli, profile.clone(), ephemeral));
    // 会话目录用完即删（WebView2 有时还抓着文件，删不掉就算了，系统临时目录会自己清）
    if ephemeral {
        let _ = std::fs::remove_dir_all(&profile);
    }
    if bare {
        if let Err(e) = &result {
            println!("\n出错了：{e:#}");
        }
        // 控制台已经被藏起来，日志都在窗口面板里；这里留几秒让人看清最后几行
        println!("\n本次运行结束，窗口将在 8 秒后关闭。");
        std::thread::sleep(std::time::Duration::from_secs(8));
    }
    result
}

async fn run(cli: Cli, profile: PathBuf, ephemeral: bool) -> anyhow::Result<()> {
    if ephemeral {
        println!("本次不保存登录态（退出即清除）。要免登录下次直接跑，请加 --keep-login。");
    } else {
        println!("登录态将保存在 {}（下次启动免登录）。", profile.display());
    }
    // 切换账号：把整份登录态清掉，让站点回到扫码/登录页。
    // 不清的话会一直用上次的 cookie 直接进，用户想换人根本没入口。
    if cli.switch {
        match std::fs::remove_dir_all(&profile) {
            Ok(()) => println!("已清空登录态 {}，启动后请重新登录。", profile.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("登录态目录本来就不存在：{}", profile.display())
            }
            Err(e) => println!("清空登录态失败（{e}），可能还有实例占着这个目录；关掉所有窗口再试。"),
        }
    }
    match cli.cmd.expect("main 已保证子命令存在") {
        Cmd::Probe { url, out, max_pages, auto_click } => {
            probe::run(&url, &out, &profile, cli.browser.as_deref(), max_pages, cli.zoom, &auto_click).await
        }
        Cmd::Learn { url, rate, max } => {
            anyhow::ensure!(
                (0.5..=4.0).contains(&rate),
                "倍速 {rate} 超出允许范围 0.5~4.0"
            );
            if rate > 2.0 {
                println!("警告：倍速 {rate} 超过平台播放器上限 2.0，心跳增量会显得不合常理，有被判异常并清空进度的风险");
            }
            let url = match resolve_entry(url) {
                Some(u) => u,
                None => anyhow::bail!(
                    "还不知道从哪开始。第一次运行请用 --url 指定「培训项目详情页」地址，例如：\n\
                     \x20 yuketang-autolearn.exe learn --url https://<你的学校域名>/pro/trainingproject/mytraining/detail/<id>\n\
                     跑过一次之后会记在 state/last_url.txt，下次直接运行就沿用；\n\
                     也可以用环境变量 YKT_ENTRY 指定。"
                ),
            };
            println!("起点：{url}");
            let adapter = load_adapter(&cli.adapters, &url)?;
            player::run(&url, &adapter, &Path::new("state"), &profile, cli.browser.as_deref(), cli.zoom, rate, max).await
        }
        Cmd::Verify { url, samples, wait, peek } => {
            let adapter = load_adapter(&cli.adapters, &url)?;
            verify::run(
                &url,
                &adapter,
                &profile,
                cli.browser.as_deref(),
                cli.zoom,
                verify::Opts {
                    wait: std::time::Duration::from_secs(wait),
                    samples,
                    peek_only: peek,
                },
            )
            .await
        }
        Cmd::Watch { url, seconds, interval, click } => {
            watch::run(&url, &profile, cli.browser.as_deref(), cli.zoom, seconds, interval, &click).await
        }
        Cmd::Routes => {
            for entry in std::fs::read_dir(&cli.adapters)? {
                let path = entry?.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let adapter = Adapter::load(&path)?;
                println!("{} （域名 {}）", adapter.name, adapter.domains.join(", "));
                for r in &adapter.routes {
                    println!(
                        "  {:<10} {:<44} {}",
                        format!("{:?}", r.kind),
                        if r.path_prefix.is_empty() { "(兜底)" } else { &r.path_prefix },
                        r.label
                    );
                }
            }
            Ok(())
        }
        Cmd::Status { url } => {
            let dir = Path::new("state");
            if !dir.exists() {
                println!("还没有状态文件，先跑 learn。");
                return Ok(());
            }
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                let store = Store::load(&path);
                if store.url.is_empty() || (url.as_deref().is_some() && !store.url.contains(&url.clone().unwrap())) {
                    continue;
                }
                let done = store.items.values().filter(|i| i.status == Status::Done).count();
                println!("{}  {}/{} 完成  [{}]", store.url, done, store.items.len(), path.display());
                for item in store.items.values() {
                    println!("  {:<12} {:>6.1}%  {} 次尝试  {}", format!("{:?}", item.status), item.pct, item.attempts, item.label);
                }
            }
            Ok(())
        }
    }
}


fn load_adapter(dir: &Path, url: &str) -> anyhow::Result<Adapter> {
    let parsed: url::Url = url.parse()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                candidates.push(path);
            }
        }
    }
    for path in &candidates {
        match Adapter::load(path) {
            Ok(adapter) if adapter.handles(&parsed) => return Ok(adapter),
            Ok(_) => {}
            Err(e) => println!("跳过适配器 {}：{e}", path.display()),
        }
    }
    // 只发一个 exe 时没有 adapters 目录，用编译进去的那份兜底。
    for (name, text) in BUILTIN_ADAPTERS {
        let adapter = Adapter::parse(text, name)?;
        if adapter.handles(&parsed) {
            return Ok(adapter);
        }
    }
    anyhow::bail!(
        "没有适配器匹配 {url}。已加载：{:?}\n请在 {} 下新建 toml，把域名加进去。",
        candidates,
        dir.display()
    )
}
