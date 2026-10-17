//! 诊断视图：打开一个地址，按固定间隔把"页面现在到底有什么"打出来。
//!
//! learn 循环之所以会反复弹回，是因为它在视频页上判定"没有媒体"。
//! 判定的依据只有 snapshot().found，而 found 为假的原因有好几种：
//! 播放器还在加载、播放器在跨域 iframe 里、页面报错停在骨架屏。
//! 这三种在日志里长得一模一样，所以先要有能把它们分开的工具。

use crate::browser::{self, eval_json, eval_json_string};
use std::path::Path;
use std::time::Duration;
use tokio::time::sleep;

/// 一次求值拿到全部判据：主文档状态、每个 iframe 能否读、媒体元素分布、可见文字。
const DIAG_JS: &str = r#"(()=>{try{
  // 播放器可能藏在 Shadow DOM 里：普通 querySelectorAll 穿不进去，这里把
  // 主文档、同源 iframe、以及所有 shadowRoot 的“文档根”一起收进来再查。
  const roots=[];
  const addRoot=(r)=>{ if(r && roots.indexOf(r)<0) roots.push(r); };
  addRoot(document);
  const frames=[];
  for (const f of Array.from(document.querySelectorAll('iframe'))) {
    let readable=false;
    try {
      const fd=f.contentDocument;
      if (fd) { readable=true; addRoot(fd); }
    } catch(e) {}
    frames.push({
      src:(f.src||'').slice(0,160), id:f.id||'',
      cls:(typeof f.className==='string'?f.className:'').slice(0,60),
      readable,
      w:Math.round(f.getBoundingClientRect().width),
      h:Math.round(f.getBoundingClientRect().height)
    });
  }
  // 递归收集 shadowRoot（含嵌套）
  const shadows=[];
  const harvest=(root)=>{
    let els=[];
    try { els=Array.from(root.querySelectorAll('*')); } catch(e) { return; }
    for (const el of els) {
      if (el.shadowRoot) { addRoot(el.shadowRoot); shadows.push(el.tagName.toLowerCase()+(el.id?'#'+el.id:'')); harvest(el.shadowRoot); }
    }
  };
  harvest(document);
  for (const r of roots.slice()) harvest(r);

  const media=[];
  for (const r of roots) {
    let list=[];
    try { list=Array.from(r.querySelectorAll('video,audio')); } catch(e){}
    for (const v of list) {
      media.push({
        root: r===document ? 'main' : (r.host ? 'shadow:'+r.host.tagName.toLowerCase() : 'iframe'),
        tag:v.tagName.toLowerCase(),
        src:(v.currentSrc||v.src||'').slice(0,140),
        dur:isFinite(v.duration)?Math.round(v.duration):-1,
        cur:Math.round((v.currentTime||0)*10)/10,
        paused:v.paused, rs:v.readyState,
        w:Math.round(v.getBoundingClientRect().width),
        h:Math.round(v.getBoundingClientRect().height)
      });
    }
  }
  const canvases=roots.reduce((n,r)=>{try{return n+r.querySelectorAll('canvas').length}catch(e){return n}},0);
  const body=((document.body&&document.body.innerText)||'').replace(/\s+/g,' ').trim().slice(0,140);
  const snap=(window.__YKT&&window.__YKT.snapshot)?window.__YKT.snapshot():null;
  return JSON.stringify({
    url:location.href, title:document.title, ready:document.readyState,
    htmlLen:(document.documentElement.outerHTML||'').length,
    iw:document.documentElement.clientWidth,
    ih:document.documentElement.clientHeight,
    dpr:window.devicePixelRatio,
    frames, media, shadows, canvases, ykt:!!window.__YKT, snap, body
  });
}catch(e){return JSON.stringify({url:location.href,error:String(e&&e.stack||e)});}})()"#;

fn j<T: serde::Serialize>(v: T) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "null".into())
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub async fn run(
    url: &str,
    profile: &Path,
    browser_folder: Option<&str>,
    zoom: f64,
    seconds: u64,
    interval: u64,
    click_sel: &[String],
) -> anyhow::Result<()> {
    let mut session = browser::Session::launch(profile, browser_folder, (1360, 900), zoom).await?;
    let page = session.new_page(url).await?;
    let player = include_str!("../assets/player.js");
    let _ = page.add_init_script(player).await;
    eval_json(&page, player).await.ok();

    println!("观察 {url}");
    println!(
        "每 {interval}s 打一条；共 {seconds}s。Ctrl+C 可提前结束。\n\
         字段：media=媒体元素数 frames=iframe 数 ykt=控制层是否就位 body=页面可见文字前 140 字"
    );

    let rounds = seconds / interval.max(1);
    let mut last = String::new();
    let mut clicks = 0usize;
    let mut clicked_url = String::new();
    for i in 0..rounds {
        sleep(Duration::from_secs(interval)).await;
        // DIAG_JS 返回的是 JSON 字符串，必须走 eval_json_string 解一层。
        // 页面正在导航时 ExecuteScript 会回 null，那是"还没求到值"，不是"页面是空的"，
        // 两者必须分开报，否则会把正常的加载错判成白屏。
        let v = match eval_json_string(&page, DIAG_JS).await {
            Ok(v) if v.get("error").is_some() => {
                let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
                println!("[{i}] 诊断脚本抛异常：{}", head(err, 300));
                continue;
            }
            Ok(v) if v.get("url").is_some() => v,
            Ok(v) => {
                // 拿不到 url 时把原始返回和一次最小求值一起打出来：
                // 区分"页面是 about:blank"、"脚本抛异常"、"求值回 null"。
                let raw = serde_json::to_string(&v).unwrap_or_default();
                let probe = eval_json_string(
                    &page,
                    "JSON.stringify({href:location.href,rs:document.readyState,\
                     bodyLen:(document.body&&document.body.innerHTML||'').length,\
                     hasYkt:!!window.__YKT})",
                )
                .await
                .map(|x| serde_json::to_string(&x).unwrap_or_default())
                .unwrap_or_else(|e| format!("<求值失败: {e}>"));
                println!("[{i}] 没拿到 url；diag={} probe={}", head(&raw, 160), head(&probe, 200));
                continue;
            }
            Err(e) => {
                println!("[{i}] 求值失败：{e}");
                continue;
            }
        };
        let url_now = v.get("url").and_then(|x| x.as_str()).unwrap_or("");
        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
        let ready = v.get("ready").and_then(|x| x.as_str()).unwrap_or("");
        let media = v.get("media").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let frames = v.get("frames").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let ykt = v.get("ykt").and_then(|x| x.as_bool()).unwrap_or(false);
        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");

        println!(
            "[{i}] {ready} media={} frames={} ykt={ykt} html={} 视口={}x{} dpr={} {title}",
            media.len(),
            frames.len(),
            v.get("htmlLen").and_then(|x| x.as_i64()).unwrap_or(0),
            v.get("iw").and_then(|x| x.as_i64()).unwrap_or(0),
            v.get("ih").and_then(|x| x.as_i64()).unwrap_or(0),
            v.get("dpr").and_then(|x| x.as_f64()).unwrap_or(0.0),
        );
        if !url_now.is_empty() {
            println!("    url: {url_now}");
        }
        for m in &media {
            println!(
                "    media {} {:.0}s/{:.0}s paused={} rs={} {}x{} {}",
                m.get("tag").and_then(|x| x.as_str()).unwrap_or("?"),
                m.get("cur").and_then(|x| x.as_f64()).unwrap_or(0.0),
                m.get("dur").and_then(|x| x.as_f64()).unwrap_or(0.0),
                m.get("paused").and_then(|x| x.as_bool()).unwrap_or(true),
                m.get("rs").and_then(|x| x.as_i64()).unwrap_or(-1),
                m.get("w").and_then(|x| x.as_i64()).unwrap_or(0),
                m.get("h").and_then(|x| x.as_i64()).unwrap_or(0),
                m.get("src").and_then(|x| x.as_str()).unwrap_or("")
            );
        }
        for f in &frames {
            println!(
                "    frame read={} {}x{} id={} {}",
                f.get("readable").and_then(|x| x.as_bool()).unwrap_or(false),
                f.get("w").and_then(|x| x.as_i64()).unwrap_or(0),
                f.get("h").and_then(|x| x.as_i64()).unwrap_or(0),
                f.get("id").and_then(|x| x.as_str()).unwrap_or(""),
                f.get("src").and_then(|x| x.as_str()).unwrap_or("")
            );
        }
        // 只在文字变化时打，避免同一句话刷屏
        if body != last {
            println!("    body: {body}");
            last = body.to_string();
        }
        if !url_now.is_empty() && url_now != url {
            println!("    地址已变：{url_now}");
        }

        // 有 media 就主动开播（站点要求先点播放，直接 play() 会被手势策略拒），
        // 并在暂停时不断补；倍速/静音由 player.js 的 enforce 每 500ms 落实一次。
        if !media.is_empty() {
            let r = eval_json(&page, "window.__YKT.start(2.0)").await.ok();
            if let Some(r) = &r {
                println!(
                    "    开播: started={} via={} 点了{}个按钮 rate={} paused={} cur={} err={}",
                    r.get("started").and_then(|x| x.as_bool()).unwrap_or(false),
                    r.get("via").and_then(|x| x.as_str()).unwrap_or("?"),
                    r.get("clicked").and_then(|x| x.as_i64()).unwrap_or(0),
                    r.get("rate").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    r.get("paused").and_then(|x| x.as_bool()).unwrap_or(true),
                    r.get("cur").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    r.get("err").and_then(|x| x.as_str()).unwrap_or("")
                );
            }
            // 走站点自己的控件落实倍速与静音，再把结果打出来（元素值 + 站点 UI 值）
            for js in ["window.__YKT.setSpeed(2.0)", "window.__YKT.mute()"] {
                if let Ok(r) = eval_json(&page, js).await {
                    let labels = r
                        .get("labels")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str())
                                .collect::<Vec<_>>()
                                .join(",")
                        })
                        .unwrap_or_default();
                    println!(
                        "    设置 {}: ok={} via={} text={} ui={} labels=[{}]",
                        js,
                        r.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
                        r.get("via").and_then(|x| x.as_str()).unwrap_or("?"),
                        r.get("text").and_then(|x| x.as_str()).unwrap_or(""),
                        r.get("ui").and_then(|x| x.as_str()).unwrap_or(""),
                        labels
                    );
                }
            }
            // 落实结果：每个媒体元素的实际倍速/静音/音量，用来验证"倍速和静音真的生效了"
            if let Ok(st) = eval_json(&page, "window.__YKT.audioState()").await {
                let want = st.get("want").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let ui = st.get("uiSpeed").and_then(|x| x.as_str()).unwrap_or("");
                let list = st.get("media").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                let desc: Vec<String> = list
                    .iter()
                    .map(|m| {
                        format!(
                            "rate={} muted={} vol={} paused={}",
                            m.get("rate").and_then(|x| x.as_f64()).unwrap_or(0.0),
                            m.get("muted").and_then(|x| x.as_bool()).unwrap_or(false),
                            m.get("volume").and_then(|x| x.as_f64()).unwrap_or(-1.0),
                            m.get("paused").and_then(|x| x.as_bool()).unwrap_or(true)
                        )
                    })
                    .collect();
                println!("    落实: 目标x{want} UI={ui} | {}", desc.join(" ; "));
            }
        }

        // 需要时替人点入口：只为把流程沿「课程→章节→视频」推下去，不做别的。
        // 每换一个地址，点序号归零，所以每一层都从该层第一个入口点起。
        if !click_sel.is_empty() {
            let here = url_now.to_string();
            if here != clicked_url {
                clicked_url = here;
                clicks = 0;
            }
            let has_media = !media.is_empty();
            if !has_media && clicks < 3 {
                if let Ok(listed) = eval_json(&page, &format!("window.__YKT.entries({})", j(click_sel))).await {
                    if let Some(items) = listed.as_array() {
                        // 跳过当前正在播的那条，否则点了等于重载自己
                        let pick = items
                            .iter()
                            .find(|it| it.get("active").and_then(|x| x.as_bool()) != Some(true));
                        if let Some(item) = pick {
                            let idx = item.get("i").and_then(|x| x.as_u64()).unwrap_or(0);
                            if let Ok(r) = eval_json(
                                &page,
                                &format!("window.__YKT.clickEntry({}, {idx})", j(click_sel)),
                            )
                            .await
                            {
                                clicks += 1;
                                println!(
                                    "    点入口: {} -> ok={}",
                                    item.get("label").and_then(|x| x.as_str()).unwrap_or("?"),
                                    r.get("ok").and_then(|x| x.as_bool()).unwrap_or(false)
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    session.close().await;
    Ok(())
}
