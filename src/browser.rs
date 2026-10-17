//! 命令层看到的"浏览器"：一个由本进程持有的 WebView2 窗口。
//!
//! 之前这一层是 CDP 客户端 + 外部 Edge，端口抢占和句柄失效的问题都出在那儿。

use crate::web;
use anyhow::Result;
use std::path::Path;

/// 页面句柄。整个应用只有一个 WebView2 实例，所以它就是那个窗口里的当前页。
pub type Page = web::Web;

pub struct Session {
    inner: web::Session,
}

impl Session {
    pub async fn launch(
        profile: &Path,
        browser_folder: Option<&str>,
        window: (u32, u32),
        zoom: f64,
    ) -> Result<Session> {
        let profile = std::path::absolute(profile)?;
        // WebView2 不会替你建父目录，路径不存在时创建控制器会直接报
        // "灾难性故障 (0x8000FFFF)"，所以先确保目录在。
        std::fs::create_dir_all(&profile)
            .map_err(|e| anyhow::anyhow!("创建登录态目录 {} 失败：{e}", profile.display()))?;
        let inner = web::Session::spawn(&profile, browser_folder, window, zoom)?;
        println!(
            "WebView2 已启动，登录态存在 {}（窗口请保持可见，最小化会让播放器停摆）",
            profile.display()
        );
        Ok(Session { inner })
    }

    /// 先拿到页面句柄、装好钩子，再由调用方导航。
    pub fn page(&self) -> Page {
        self.inner.page()
    }

    pub async fn new_page(&mut self, url: &str) -> Result<Page> {
        self.inner.navigate(url).await
    }

    pub async fn close(self) {
        self.inner.close().await;
    }
}

pub async fn eval_json(page: &Page, expr: &str) -> Result<serde_json::Value> {
    page.eval(expr).await
}

/// 求值一段返回 JSON 字符串的 JS，解析成 Value。避免序列化复杂 DOM 对象。
pub async fn eval_json_string(page: &Page, expr: &str) -> Result<serde_json::Value> {
    page.eval_json_string(expr).await
}
