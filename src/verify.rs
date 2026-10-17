use crate::adapter::Adapter;
use crate::browser::{self, eval_json, eval_json_string, Page};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::time::sleep;

const NETHOOK_JS: &str = include_str!("../assets/nethook.js");
const TICK: Duration = Duration::from_millis(2500);

pub struct Opts {
    pub wait: Duration,
    pub samples: usize,
    pub peek_only: bool,
}

pub async fn run(
    start_url: &str,
    adapter: &Adapter,
    profile: &Path,
    browser_path: Option<&str>,
    zoom: f64,
    opts: Opts,
) -> anyhow::Result<()> {
    let mut session = browser::Session::launch(profile, browser_path, (1360, 900), zoom).await?;
    let page = session.new_page(start_url).await?;
    // init script 只对之后的导航生效，已经加载完的这一页立刻手动挂一次
    let _ = page.add_init_script(NETHOOK_JS).await;
    eval_json(&page, NETHOOK_JS)
        .await
        .map_err(|e| anyhow::anyhow!("注入抓包钩子失败：{e}"))?;

    let patterns: Vec<String> = adapter
        .watch
        .heartbeat_url
        .iter()
        .chain(adapter.watch.progress_url.iter())
        .cloned()
        .collect();

    println!("钩子已注入（适配器 {}）。", adapter.name);
    println!("现在请在打开的窗口里点进一个视频，让它播 20~30 秒，让心跳自己发出去。");
    println!("这一步只读请求，不发任何东西。找 heartbeat 接口的 URL 片段：{}", patterns.join(", "));

    let found = wait_for_capture(&page, &patterns, &adapter.watch.heartbeat_url, opts.wait).await?;
    let hb = found.heartbeat.expect("循环只在抓到心跳后返回");

    println!("\n== 抓到的心跳 ==");
    println!("URL   : {}", s(&hb, "url"));
    println!("方法  : {}  状态: {}", s(&hb, "method"), d(&hb, "status"));
    println!("Origin: {}  同源: {}", s(&hb, "origin"), location_origin(&hb));
    if let Some(ct) = hb.get("headers").and_then(|h| h.get("content-type")).and_then(|v| v.as_str()) {
        println!("Content-Type: {ct}");
    }
    let body = parse_payload(&s(&hb, "body")).ok_or_else(|| {
        anyhow::anyhow!(
            "心跳请求体不是可解析的 JSON，回放前要先复刻它的编码格式。原始前 400 字：{}",
            s(&hb, "body").chars().take(400).collect::<String>()
        )
    })?;
    let (path, samples_arr) = find_samples_path(&body, &mut Vec::new()).ok_or_else(|| {
        anyhow::anyhow!("心跳 JSON 里找不到带 cp 字段的样本数组，字段可能已改版：{}", head(&body, 400))
    })?;
    let (cp_step, ts_step, step_src) = derive_step(&samples_arr);
    println!(
        "样本数组: {} 条，字段路径 {}，步进 cp+{cp_step:.1}s / ts+{ts_step:.0}ms（{step_src}）",
        samples_arr.len(),
        path.join(".")
    );
    println!("最后一条样本：{}", head(&samples_arr[samples_arr.len() - 1], 600));

    let progress = found
        .progress
        .as_ref()
        .map(|p| (s(p, "url"), p.get("headers").cloned().unwrap_or(json!({}))))
        .filter(|(u, _)| !u.is_empty());
    match &progress {
        Some((u, _)) => println!("进度接口: {u}"),
        None => println!(
            "进度接口: 没抓到（等视频页自己再发一次，或把 adapters/*.toml 的 progress_url 补上你们的变体路径）"
        ),
    }

    if opts.peek_only {
        println!("\n--peek：只看 payload，不回放。");
        session.close().await;
        return Ok(());
    }

    let before = match progress.as_ref() {
        Some((u, h)) => Some(read_metrics(&page, u, h).await?),
        None => None,
    };
    if let Some(m) = &before {
        println!("\n回放前：{m}");
    }

    let replay = build_replay(&body, &path, &samples_arr, cp_step, ts_step, opts.samples);
    let sent = find_samples_path(&replay, &mut Vec::new())
        .map(|(_, a)| a.len())
        .unwrap_or(0);
    println!(
        "\n准备回放 {sent} 条新样本（cp 从 {:.1}s 起，每步 +{cp_step:.1}s）。\n\
         提示：想比对干净，先把页面里的视频暂停，免得它自己的心跳混进来。",
        num(samples_arr[samples_arr.len() - 1].get("cp").unwrap_or(&Value::Null)).unwrap_or(0.0)
    );
    let resp = call(
        &page,
        &json!({
            "url": s(&hb, "url"),
            "method": "POST",
            "headers": hb.get("headers").cloned().unwrap_or(json!({})),
            "body": serde_json::to_string(&replay)?,
        }),
    )
    .await?;
    println!(
        "\n回放响应：{} {}",
        d(&resp, "status"),
        s(&resp, "resp").chars().take(400).collect::<String>()
    );
    let resp_text = s(&resp, "resp");
    if resp_text.contains("anomaly") {
        println!("服务端回了 anomaly：这是异常判定通道，进度有被清零的风险，不建议走纯 API。");
    }
    if resp_text.contains("Expected available") {
        println!("服务端限流（Expected available in N second）：等一会再重试，别把 samples 调大。");
    }

    let after = match progress.as_ref() {
        Some((u, h)) => Some(read_metrics(&page, u, h).await?),
        None => None,
    };
    session.close().await;

    let (before, after) = match (before, after) {
        (Some(b), Some(a)) => (b, a),
        _ => {
            println!(
                "\n无法判定：没抓到进度接口就没法比对。请在视频页多等一次轮询，或补 progress_url 配置。\n\
                 回放用的 payload 已经存到 dumps/verify-heartbeat.json 供人工核对。"
            );
            dump_payload(&replay)?;
            return Ok(());
        }
    };
    println!("回放后：{after}");

    if after.grew(&before) {
        println!(
            "\n结论：心跳通道有效。进度只认 cp/ts 序列，{sent} 条样本把 watch 从 {:.0}s 推到 {:.0}s。\n\
             可以把视频这一段换成纯 API 上报，CDP 只留着做登录和它覆盖不到的页面。",
            before.watch,
            after.watch
        );
    } else if after.rate - before.rate > f64::EPSILON {
        println!("\n结论：rate 有变化但 watch_length 没涨，步进可能与服务端认可的粒度不符，调 --samples 再试。");
    } else {
        println!(
            "\n结论：进度没动。可能是字段漂移（cp/ts 与 video_id 不成对）、该视频已过考核截止时间、\n\
             或这个变体校验了额外字段。把上面的 payload 和响应拿去和 Niuwh/yuketang-jiaoben、\n\
             heyblackC/yuketangHelper 的字段表逐一对，再决定要不要走 API。"
        );
        dump_payload(&replay)?;
    }
    Ok(())
}

#[derive(Default)]
struct Captured {
    heartbeat: Option<Value>,
    progress: Option<Value>,
}

async fn wait_for_capture(
    page: &Page,
    patterns: &[String],
    heartbeat_patterns: &[String],
    wait: Duration,
) -> anyhow::Result<Captured> {
    let deadline = Instant::now() + wait;
    let mut out = Captured::default();
    loop {
        sleep(TICK).await;
        let hits = match dump_hits(page, patterns).await {
            Ok(v) => v,
            Err(e) => {
                println!("抓包轮询失败（页面可能正在跳转）：{e}");
                continue;
            }
        };
        for rec in &hits {
            let url = s(rec, "url");
            if out.heartbeat.is_none()
                && heartbeat_patterns.iter().any(|p| url.contains(p))
                && s(rec, "method").to_uppercase() == "POST"
                && parse_payload(&s(rec, "body")).is_some()
            {
                out.heartbeat = Some(rec.clone());
            }
            if out.progress.is_none()
                && patterns
                    .iter()
                    .filter(|p| !heartbeat_patterns.iter().any(|h| h.as_str() == p.as_str()))
                    .any(|p| url.contains(p))
                && s(rec, "resp").contains("rate")
            {
                out.progress = Some(rec.clone());
            }
        }
        if out.heartbeat.is_some() && out.progress.is_some() {
            return Ok(out);
        }
        if out.heartbeat.is_some() && Instant::now() > deadline {
            println!("心跳抓到了，但进度接口一直没出现，先按现有记录继续。");
            return Ok(out);
        }
        if Instant::now() > deadline {
            anyhow::bail!(
                "{:?} 内没抓到带 JSON 请求体的心跳。确认：视频真在播（不是暂停/黑屏）、\n\
                 播放器是否把请求发在 iframe 里（当前钩子只装在主文档），\n\
                 以及 adapters/*.toml 的 watch.heartbeat_url 是否匹配你们学校的变体路径。",
                wait
            );
        }
        print!(".");
    }
}

async fn dump_hits(page: &Page, patterns: &[String]) -> anyhow::Result<Vec<Value>> {
    let v = eval_json_string(
        page,
        &format!("window.__YKT_NET.dump({})", serde_json::to_string(patterns)?),
    )
    .await?;
    Ok(v.get("hits").and_then(|h| h.as_array()).cloned().unwrap_or_default())
}

/// 回放请求走页面自己的 fetch，结果写进 __YKT_NET.last 由这里轮询取，
/// 这样不依赖 Runtime.evaluate 的 awaitPromise 行为。
async fn call(page: &Page, spec: &Value) -> anyhow::Result<Value> {
    eval_json_string(page, &format!("window.__YKT_NET.send({})", spec)).await?;
    for _ in 0..40 {
        sleep(Duration::from_millis(400)).await;
        let v = eval_json_string(page, "window.__YKT_NET.last()").await?;
        if v.get("state").and_then(|x| x.as_str()) != Some("pending") {
            anyhow::ensure!(v.get("state").and_then(|x| x.as_str()) != Some("error"), "页面内请求失败：{}", head(&v, 300));
            return Ok(v);
        }
    }
    Ok(json!({ "state": "timeout", "status": null, "resp": "" }))
}

async fn read_metrics(page: &Page, url: &str, headers: &Value) -> anyhow::Result<Metrics> {
    let resp = call(page, &json!({ "url": url, "method": "GET", "headers": headers })).await?;
    let body = s(&resp, "resp");
    let parsed = parse_payload(&body).ok_or_else(|| {
        anyhow::anyhow!("进度接口返回的不是 JSON（status {}），拿它做前后比对没有意义：{}", d(&resp, "status"), body.chars().take(200).collect::<String>())
    })?;
    Ok(Metrics::of(&parsed))
}

#[derive(Debug, Clone, Copy, Default)]
struct Metrics {
    rate: f64,
    watch: f64,
    completed: usize,
    videos: usize,
}

impl Metrics {
    /// 进度接口按 video_id 建表返回多个条目，这里只取总量做前后比较，避免绑死某个字段名。
    fn of(v: &Value) -> Self {
        let mut m = Self::default();
        walk(v, &mut |o| {
            let rate = o.get("rate").and_then(|x| x.as_f64());
            let watch = o
                .get("watch_length")
                .and_then(|x| x.as_f64())
                .or_else(|| o.get("current_time").and_then(|x| x.as_f64()));
            if rate.is_none() && watch.is_none() {
                return;
            }
            m.videos += 1;
            if let Some(r) = rate {
                m.rate += r;
            }
            if let Some(w) = watch {
                m.watch = m.watch.max(w);
            }
            if o.get("completed").and_then(|x| x.as_bool()) == Some(true) {
                m.completed += 1;
            }
        });
        m
    }

    fn grew(&self, other: &Self) -> bool {
        self.watch > other.watch + 0.5 || self.completed > other.completed
    }
}

impl std::fmt::Display for Metrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.videos == 0 {
            return write!(f, "（响应里没有找到 rate/watch_length 字段）");
        }
        write!(
            f,
            "rate 合计={:.3}  watch={:.0}s  completed={}/{} 项",
            self.rate, self.watch, self.completed, self.videos
        )
    }
}

fn walk<'a>(v: &'a Value, visit: &mut dyn FnMut(&'a Value)) {
    match v {
        Value::Object(map) => {
            visit(v);
            for val in map.values() {
                walk(val, visit);
            }
        }
        Value::Array(items) => {
            for val in items {
                walk(val, visit);
            }
        }
        _ => {}
    }
}

fn find_samples_path(v: &Value, path: &mut Vec<String>) -> Option<(Vec<String>, Vec<Value>)> {
    if let Value::Object(map) = v {
        for (k, val) in map {
            if let Value::Array(items) = val {
                if items.iter().any(|i| i.get("cp").is_some()) {
                    let mut p = path.clone();
                    p.push(k.clone());
                    return Some((p, items.clone()));
                }
            }
            path.push(k.clone());
            let hit = find_samples_path(val, path);
            path.pop();
            if let Some(h) = hit {
                return Some(h);
            }
        }
    }
    None
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn derive_step(arr: &[Value]) -> (f64, f64, &'static str) {
    if arr.len() >= 2 {
        let a = &arr[arr.len() - 2];
        let b = &arr[arr.len() - 1];
        let cp = num(b.get("cp").unwrap_or(&Value::Null)).unwrap_or(0.0)
            - num(a.get("cp").unwrap_or(&Value::Null)).unwrap_or(0.0);
        let ts = num(b.get("ts").unwrap_or(&Value::Null)).unwrap_or(0.0)
            - num(a.get("ts").unwrap_or(&Value::Null)).unwrap_or(0.0);
        if cp > 0.0 && ts > 0.0 {
            return (cp, ts, "由相邻两条样本推导");
        }
    }
    (20.0, 15000.0, "样本不足两条，用默认 20s/15000ms")
}

/// 服务端认的是 cp/ts 成比例往后爬，所以只造新样本、不重发旧的。
fn build_replay(
    body: &Value,
    path: &[String],
    arr: &[Value],
    cp_step: f64,
    ts_step: f64,
    samples: usize,
) -> Value {
    let mut out = body.clone();
    let last = &arr[arr.len() - 1];
    let cp0 = num(last.get("cp").unwrap_or(&json!(0))).unwrap_or(0.0);
    let ts0 = num(last.get("ts").unwrap_or(&json!(0))).unwrap_or(0.0);
    let duration = num(last.get("d").unwrap_or(&json!(0))).unwrap_or(0.0);
    let ts_is_string = last.get("ts").map(|x| x.is_string()).unwrap_or(false);

    let fresh: Vec<Value> = (1..=samples)
        .map(|i| {
            let mut s = last.clone();
            let cp = cp0 + cp_step * i as f64;
            let cp = if duration > 0.0 { cp.min(duration) } else { cp };
            let ts = ts0 + ts_step * i as f64;
            s["cp"] = json!(round1(cp));
            s["ts"] = if ts_is_string {
                json!(format!("{}", ts as i64))
            } else {
                json!(ts as i64)
            };
            s
        })
        .take_while(|s| num(s.get("cp").unwrap_or(&json!(0))).unwrap_or(0.0) > 0.0)
        .collect();

    let pointer = format!("/{}", path.iter().map(|k| k.replace('~', "~0").replace('/', "~1")).collect::<Vec<_>>().join("/"));
    if let Some(slot) = out.pointer_mut(&pointer) {
        *slot = Value::Array(fresh);
    }
    out
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn dump_payload(v: &Value) -> anyhow::Result<()> {
    let dir = Path::new("dumps");
    std::fs::create_dir_all(dir)?;
    let file = dir.join("verify-heartbeat.json");
    std::fs::write(&file, serde_json::to_string_pretty(v)?)?;
    println!("payload 已存：{}", file.display());
    Ok(())
}

fn parse_payload(raw: &str) -> Option<Value> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    // 表单编码时字段值里塞着 JSON 字符串，先解 %xx 和 + 再取最外层花括号
    let decoded = percent_decode(t);
    if let Ok(v) = serde_json::from_str::<Value>(decoded.trim()) {
        return Some(v);
    }
    let (a, b) = (decoded.find('{')?, decoded.rfind('}')?);
    if b <= a {
        return None;
    }
    serde_json::from_str(&decoded[a..=b]).ok()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn d(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_i64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".into())
}

fn head(v: &Value, n: usize) -> String {
    let text = serde_json::to_string(v).unwrap_or_else(|_| "<非 JSON>".into());
    text.chars().take(n).collect()
}

fn location_origin(rec: &Value) -> String {
    let origin = s(rec, "origin");
    if origin.is_empty() {
        "未知".into()
    } else {
        origin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({"heart_data":[
            {"cp":20,"ts":"15000","d":100,"v":"vid","sp":1.25,"u":"1"},
            {"cp":40,"ts":"30000","d":100,"v":"vid","sp":1.25,"u":"1"}
        ]})
    }

    #[test]
    fn 步进从相邻样本推导() {
        let arr = body()["heart_data"].as_array().unwrap().clone();
        assert_eq!(derive_step(&arr).0, 20.0);
        assert_eq!(derive_step(&arr).1, 30000.0 - 15000.0);
        assert_eq!(derive_step(&[json!({"cp":1})]), (20.0, 15000.0, "样本不足两条，用默认 20s/15000ms"));
    }

    #[test]
    fn 回放只造新样本并保持字段类型() {
        let b = body();
        let (path, arr) = find_samples_path(&b, &mut Vec::new()).unwrap();
        assert_eq!(path, vec!["heart_data".to_string()]);
        let out = build_replay(&b, &path, &arr, 20.0, 15000.0, 5);
        let fresh = out["heart_data"].as_array().unwrap();
        assert_eq!(fresh.len(), 5);
        // 从最后一条 cp=40 往后爬，超过总时长 d=100 的部分钳到 100
        let cps: Vec<f64> = fresh.iter().map(|x| x["cp"].as_f64().unwrap()).collect();
        assert_eq!(cps, vec![60.0, 80.0, 100.0, 100.0, 100.0]);
        // 原 payload 里 ts 是字符串，回放必须保持字符串，否则服务端类型校验会挂
        assert!(fresh[0]["ts"].is_string(), "{:?}", fresh[0]["ts"]);
        assert_eq!(fresh[0]["ts"].as_str().unwrap(), "45000");
        assert_eq!(fresh[0]["v"].as_str().unwrap(), "vid");
        assert_eq!(fresh[0]["sp"].as_f64().unwrap(), 1.25);
    }

    #[test]
    fn 表单编码里嵌套的_json_也能解出来() {
        let raw = "data=%7B%22heart_data%22%3A%5B%7B%22cp%22%3A20%7D%5D%7D";
        let v = parse_payload(raw).unwrap();
        assert_eq!(v["heart_data"][0]["cp"].as_f64(), Some(20.0));
        assert_eq!(find_samples_path(&v, &mut Vec::new()).unwrap().0, vec!["heart_data".to_string()]);
    }

    #[test]
    fn 进度指标按_video_条目聚合() {
        let v = json!({"code":0,"data":{
            "77":{"rate":0.5,"watch_length":300,"completed":false},
            "78":{"rate":1.0,"watch_length":600,"completed":true}
        }});
        let m = Metrics::of(&v);
        assert_eq!(m.videos, 2);
        assert!((m.rate - 1.5).abs() < 1e-9);
        assert_eq!(m.watch, 600.0);
        assert_eq!(m.completed, 1);
        assert!(m.grew(&Metrics { rate: 1.0, watch: 300.0, completed: 1, videos: 2 }));
        assert!(!Metrics::of(&json!({"code":0})).grew(&m));
    }
}
