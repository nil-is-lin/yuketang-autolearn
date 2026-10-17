use serde::Deserialize;
use std::path::Path;

/// 参考 OCS 的 Project/Script/matches 结构：域名 + 路由前缀决定这段页面交给哪个处理器。
/// 选择器全部外置到 TOML，平台改版时改配置而不是重编译。
#[derive(Debug, Clone, Deserialize)]
pub struct Adapter {
    pub name: String,
    pub domains: Vec<String>,
    pub routes: Vec<Route>,
    #[serde(default)]
    pub watch: Watch,
}

/// 抓包/回放时用来认出心跳和进度接口的 URL 片段。不同雨课堂变体（v1 mooc-api、长江、学校镜像）
/// 路径不一样，所以做成配置而不是写死。
#[derive(Debug, Clone, Deserialize)]
pub struct Watch {
    #[serde(default = "default_heartbeat_url")]
    pub heartbeat_url: Vec<String>,
    #[serde(default = "default_progress_url")]
    pub progress_url: Vec<String>,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            heartbeat_url: default_heartbeat_url(),
            progress_url: default_progress_url(),
        }
    }
}

fn default_heartbeat_url() -> Vec<String> {
    vec!["video-log/heartbeat".into(), "/heartbeat/".into()]
}

fn default_progress_url() -> Vec<String> {
    vec![
        "video-log/get_video_watch_progress".into(),
        "watch_progress".into(),
        "video-log/detail".into(),
    ]
}

#[derive(Debug, Clone, Deserialize)]
pub struct Route {
    pub label: String,
    /// 命中用的路径前缀（不含 query），空字符串表示同域名下兜底
    pub path_prefix: String,
    /// 这一页要做的事：entry=列出可点的学习项, learn=播放并等完成, unknown=只探测
    pub kind: RouteKind,
    /// 章节/课程入口的选择器，按顺序尝试
    #[serde(default)]
    pub entry_selectors: Vec<String>,
    /// 页面内浮层、确认框的选择器，出现就点掉
    #[serde(default)]
    pub dismiss_selectors: Vec<String>,
    /// 完成判定阈值（观看百分比）
    #[serde(default = "default_complete_pct")]
    pub complete_pct: f64,
}

fn default_complete_pct() -> f64 {
    99.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    Entry,
    Learn,
    Unknown,
}

impl Adapter {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取适配器 {} 失败：{e}", path.display()))?;
        Self::parse(&text, &path.display().to_string())
    }

    /// 从文本解析；origin 只用于报错信息（文件路径或内置名）。
    pub fn parse(text: &str, origin: &str) -> anyhow::Result<Self> {
        let adapter: Adapter = toml::from_str(text)
            .map_err(|e| anyhow::anyhow!("适配器 {origin} 格式错误：{e}"))?;
        anyhow::ensure!(!adapter.routes.is_empty(), "适配器 {origin} 没有任何路由");
        Ok(adapter)
    }

    pub fn handles(&self, url: &url::Url) -> bool {
        let host = url.host_str().unwrap_or("");
        self.domains
            .iter()
            .any(|d| host == d.as_str() || host.ends_with(&format!(".{d}")))
    }

    /// 取最长前缀匹配，避免 /trainingproject/mytraining/index 被 detail 的路由抢走
    pub fn match_route(&self, url: &url::Url) -> Option<&Route> {
        let hashless = url.as_str().split('#').next().unwrap_or("").to_string();
        self.routes
            .iter()
            .filter(|r| !r.path_prefix.is_empty() && hashless.contains(&r.path_prefix))
            .max_by_key(|r| r.path_prefix.len())
            .or_else(|| self.routes.iter().find(|r| r.path_prefix.is_empty()))
    }
}
