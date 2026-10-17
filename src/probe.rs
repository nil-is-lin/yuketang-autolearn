use crate::browser::{self, eval_json, eval_json_string};
use std::path::Path;
use std::time::Duration;
use tokio::time::sleep;

const PROBE_JS: &str = include_str!("../assets/probe.js");
const PLAYER_JS: &str = include_str!("../assets/player.js");

/// 把选择器串塞进 JS 参数位
fn j<T: serde::Serialize>(v: T) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "null".into())
}

/// 打开真实页面，URL 一变就落一份结构快照。
/// 这样不需要谁把账号交出来，跑一遍就能拿到登录后的 DOM 和接口清单。
pub async fn run(
    start_url: &str,
    out_dir: &Path,
    profile: &Path,
    browser_folder: Option<&str>,
    max_pages: usize,
    zoom: f64,
    auto_click: &[String],
) -> anyhow::Result<()> {
    std::fs::create_dir_all(out_dir).map_err(|e| anyhow::anyhow!("创建输出目录 {} 失败：{e}", out_dir.display()))?;
    let auto_zoom = zoom <= 0.0;
    println!("profile={} out={}", profile.display(), out_dir.display());
    let session = browser::Session::launch(profile, browser_folder, (1360, 900), zoom).await?;
    // 先挂 init script：probe.js 开头的 EME 钩子要在页面自己的脚本之前装上，
    // 否则 DRM 申请发生在采集之前就抓不到了。
    let page = session.page();
    if let Err(e) = page.add_init_script(&format!("{PROBE_JS}\n{PLAYER_JS}")).await {
        println!("钩子没装上（不影响主流程）：{e}");
    }
    // 导航报错不代表窗口里没东西：SPA 常把首屏重定向报成未完成，
    // 这里只警告，让人工登录和点击继续下去。
    if let Err(e) = page.goto(start_url).await {
        println!("首次导航报了问题：{e}\n窗口仍然开着，可以手动在地址栏访问，探测照常进行。");
    } else {
        println!("已进入 {start_url}");
    }

    println!("浏览器已打开。请在里面正常点进课程、章节、视频页。");
    println!("每进入一个新地址就会存一份快照到 {}，按 Ctrl+C 结束。", out_dir.display());
    // init script 只对之后的文档生效，当前这页手动补一次控制层
    eval_json(&page, PLAYER_JS).await.ok();

    // 播放器是 URL 变了之后才挂出来的，所以每个地址先记一条，等媒体数变大再补采一次
    let mut seen: Vec<(String, usize)> = Vec::new();
    let mut clicked: std::collections::HashMap<String, usize> = Default::default();
    // 自动遍历跑完（当前地址没有下一个入口，且不再产生新快照）后要自己收摊，
    // 否则无人值守时这个循环会一直空转。
    let mut idle = 0usize;
    let mut last_zoom = if zoom > 0.0 { zoom } else { 1.0 };
    loop {
        let value = match eval_json_string(&page, PROBE_JS).await {
            Ok(v) => v,
            Err(e) => {
                println!("页面已关闭或不可读，结束探测：{e}");
                break;
            }
        };
        let url = value.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
        if url.is_empty() {
            println!("探测返回空：{}", serde_json::to_string(&value).unwrap_or_default().chars().take(200).collect::<String>());
        }
        // SPA 常在同一个地址里换内容，所以按 地址+标题 去重
        let title = value.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let key = format!("{url} | {title}");
        let media_now = value.get("media").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0);
        let pos = seen.iter().position(|(k, _)| *k == key);
        let fresh = pos.is_none();
        let grew = pos.is_some_and(|i| media_now > seen[i].1);
        if !url.is_empty() && (fresh || grew) {
            if fresh {
                seen.push((key.clone(), media_now));
            } else {
                seen[pos.unwrap()].1 = media_now;
            }
            let file = out_dir.join(format!("{:08x}.json", fnv(&key)));
            std::fs::write(&file, serde_json::to_string_pretty(&value)?)
                .map_err(|e| anyhow::anyhow!("写快照 {} 失败：{e}", file.display()))?;

            let frames = value.get("frames").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0);
            let clickables = value.get("clickables").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0);
            let buttons = value.get("buttons").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0);
            println!(
                "{} [{}] {} 媒体={} iframe={} 可点项={} 按钮={} -> {}",
                if fresh { "新增" } else { "补采" },
                seen.len(),
                trim(&url),
                media_now,
                frames,
                clickables,
                buttons,
                file.file_name().and_then(|n| n.to_str()).unwrap_or("?")
            );
            if let Some(list) = value.get("buttons").and_then(|b| b.as_array()) {
                let mut by_text: std::collections::BTreeMap<String, usize> = Default::default();
                for b in list {
                    if let Some(t) = b.get("txt").and_then(|t| t.as_str()) {
                        *by_text.entry(t.to_string()).or_default() += 1;
                    }
                }
                let summary: Vec<String> = by_text
                    .iter()
                    .map(|(t, n)| format!("{t}×{n}"))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .take(14)
                    .collect();
                println!("    按钮：{}", summary.join("  "));
            }
            if seen.len() >= max_pages {
                println!("已达到 {max_pages} 个页面的上限，停止探测。");
                break;
            }
        }
        // 拖边界、最大化之后重算一次，表格要始终放得下；值没变就不刷屏
        if auto_zoom {
            match page.fit_zoom().await {
                Ok(z) => {
                    if (z - last_zoom).abs() > 0.01 {
                        println!("自适应缩放 {z:.2}");
                        last_zoom = z;
                    }
                }
                Err(e) => println!("自适应缩放量不出来：{e}"),
            }
        }
        // 自动遍历：每进一个新地址，就按序号点下一个入口，省得人守着鼠标
        if !auto_click.is_empty() && !url.is_empty() && seen.len() < max_pages {
            let listed = eval_json(&page, &format!("window.__YKT.entries({})", j(auto_click)))
                .await
                .unwrap_or(serde_json::Value::Null);
            let list = listed.as_array().cloned().unwrap_or_default();
            let n = clicked.entry(key.clone()).or_insert(0);
            if let Some(item) = list.get(*n) {
                let idx = item.get("i").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let label: String = item
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(46)
                    .collect();
                *n += 1;
                println!("自动点击 #{idx} {label}");
                let _ = eval_json(&page, &format!("window.__YKT.clickEntry({}, {idx})", j(auto_click))).await;
                sleep(Duration::from_secs(4)).await;
                continue;
            }
        }
        if fresh || grew {
            idle = 0;
        } else {
            idle += 1;
        }
        // 阈值放宽到约 60 秒：留够人工登录、扫码的时间，又不会无人值守时永远空转
        if !auto_click.is_empty() && !seen.is_empty() && idle >= 40 {
            println!("当前页面已没有下一个入口，自动遍历结束（共 {} 份快照）。", seen.len());
            break;
        }
        sleep(Duration::from_millis(1500)).await;
    }

    session.close().await;
    Ok(())
}

fn fnv(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

fn trim(url: &str) -> String {
    if url.len() > 90 {
        format!("{}…", &url[..90])
    } else {
        url.to_string()
    }
}
