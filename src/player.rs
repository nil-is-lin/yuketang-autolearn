use crate::adapter::Adapter;
use crate::browser::{self, eval_json};
use crate::state::{Status, Store};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::time::sleep;

const PLAYER_JS: &str = include_str!("../assets/player.js");
/// 首次运行的设置页：填地址、选倍速，点「开始刷课」。
const SETUP_HTML: &str = include_str!("../assets/setup.html");

/// 让页面自己去问服务端"我是谁"。写进 window.__YKT_LOGIN 供 Rust 轮询。
/// 用异步 fetch + 全局变量，而不是直接返回 Promise——我们的求值接口不等 Promise。
const LOGIN_PROBE_JS: &str = r#"(() => {
  window.__YKT_LOGIN = { state: 'pending' };
  const done = (v) => { window.__YKT_LOGIN = v; };
  (async () => {
    // 不同变体接口不同，挨个试；只要有任意一个明确表示"未登录"就判定未登录
    const urls = ['/v2/api/web/userinfo', '/edu_admin/get_user_basic_info/'];
    for (const u of urls) {
      try {
        const r = await fetch(u, { credentials: 'include' });
        const t = (await r.text()).slice(0, 2000);
        let j = null;
        try { j = JSON.parse(t); } catch (e) {}
        const code = j && (j.code ?? j.errcode ?? j.err_code);
        const unauth =
          r.status === 401 ||
          code === 401 || code === -1 || code === 40001 || code === 10001 ||
          /"?(login|passport|未登录|请先登录)"?/i.test(t.slice(0, 300));
        if (unauth) return done({ state: 'no', via: u, status: r.status, code: code ?? null });
        // 能拿到用户信息就算登录成功
        if (r.ok && j && (j.data || j.user || j.name || j.user_id)) {
          return done({ state: 'yes', via: u, status: r.status });
        }
      } catch (e) {}
    }
    done({ state: 'unknown' });
  })();
  return 'started';
})()"#;

#[derive(Debug)]
struct Cfg {
    rate: f64,
    max_items: usize,
    tick: Duration,
    stall_limit: Duration,
}

pub async fn run(
    start_url: Option<String>,
    adapters_dir: &Path,
    state_dir: &Path,
    profile: &Path,
    browser_path: Option<&str>,
    zoom: f64,
    rate: f64,
    max_items: usize,
) -> anyhow::Result<()> {
    let mut session = browser::Session::launch(profile, browser_path, (1360, 900), zoom).await?;
    let page = session.page();

    // 没有地址就先把设置页摆出来：地址、倍速、要不要记住登录，都让用户点。
    // 用 WebView2 自己渲染 HTML，不引 GUI 框架，也不用写配置文件。
    let mut chosen_rate = rate;
    let start_url = match start_url {
        Some(u) => u,
        None => {
            let cfg_path = Store::path(state_dir, "config");
            let saved = crate::state::Config::load(&cfg_path);
            // 没存过地址时给个默认域名，用户只要把后面的课程编号补上就行
            const DEFAULT_ENTRY_BASE: &str = "https://acgecfd.yuketang.cn";
            let prefill = if saved.url.is_empty() {
                DEFAULT_ENTRY_BASE.to_string()
            } else {
                saved.url.clone()
            };
            let html = SETUP_HTML.replace("__PREFILL__", &prefill)
                .replace("__RATE__", &format!("{}", saved.rate));
            page.show_html(&html).await?;
            println!("已打开设置页：确认倍速/登录方式后，点「开始刷课」。");
            let mut picked: Option<(String, f64)> = None;
            loop {
                sleep(Duration::from_millis(400)).await;
                let Some(raw) = page.take_message() else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
                    continue;
                };
                let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
                if action == "quit" {
                    println!("用户选择退出。");
                    session.close().await;
                    return Ok(());
                }
                if action != "start" {
                    continue;
                }
                let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                if url.is_empty() {
                    continue;
                }
                let r = v.get("rate").and_then(|x| x.as_f64()).unwrap_or(2.0);
                let keep = v.get("keepLogin").and_then(|x| x.as_bool()).unwrap_or(false);
                let cfg = crate::state::Config {
                    url: url.clone(),
                    rate: r,
                    keep_login: keep,
                };
                let _ = cfg.save(&cfg_path);
                picked = Some((url, r));
                break;
            }
            let (url, r) = picked.expect("循环只在拿到地址后返回");
            chosen_rate = r;
            url
        }
    };
    let adapter = Adapter::load_for(adapters_dir, &start_url)?;
    anyhow::ensure!(adapter.handles(&start_url.parse()?), "当前适配器 {0} 不匹配 {start_url}", adapter.name);

    let cfg = Cfg {
        rate: chosen_rate,
        // 0 = 刷到没有下一项为止（双击 exe 的默认）
        max_items: if max_items == 0 { usize::MAX } else { max_items },
        // 目录页只需要点入口，轮询要快；播放中才用慢节拍，免得日志刷屏。
        // 之前统一 5 秒，前两级目录白等 20 秒才见到视频，很没必要。
        tick: Duration::from_secs(2),
        stall_limit: Duration::from_secs(60),
    };

    let key = format!("{:08x}", fnv(&start_url));
    let state_path = Store::path(state_dir, &key);
    let mut store = Store::load(&state_path);
    store.url = start_url.to_string();
    // 新一次运行不继承上一次的重试计数：上次可能因为改版/网络问题失败过，
    // 计数留在文件里会让这次一开局就把所有入口当"试过上限"跳过，连视频页都进不去。
    let revived = store.reset_stalled(99.0);
    if revived > 0 {
        println!("已重置 {revived} 项的历史重试计数（本次重新尝试）");
        store.save(&state_path)?;
    }
    // 双击 exe 时不带参数也要能跑：把这次用的地址记下来当默认值
    if std::fs::create_dir_all(state_dir).is_ok() {
        let _ = std::fs::write(state_dir.join("last_url.txt"), &start_url);
    }

    let page = session.new_page(&start_url).await?;
    let _ = page.add_init_script(PLAYER_JS).await;
    eval_json(&page, PLAYER_JS).await.ok();

    // 站点对课程页并不强制登录，直接进去也能看——但我们要求"登录后才能刷"，
    // 所以这里主动问一次服务端身份，未登录就停下来等用户登录。
    let origin = match start_url.parse::<url::Url>() {
        Ok(u) => format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")),
        Err(_) => String::new(),
    };
    ensure_login(&page, &origin).await;

    println!(
        "开始：{} 倍速，完成阈值按路由配置。状态文件 {}",
        cfg.rate,
        state_path.display()
    );

    // 双击 exe 的场景：profile 里没登录态时页面会跳到登录页，这里等人扫完码再开跑。
    let start_route = adapter.match_route(&start_url.parse()?);
    let start_sel = start_route
        .map(|r| r.entry_selectors.clone())
        .unwrap_or_default();
    println!("窗口里如果是登录页，请扫码登录；登录后自动开始。");
    println!("要换账号：关掉窗口后加 --switch 重新运行，会清掉已保存的登录态。");
    let mut told_login = false;
    for _ in 0..300 {
        let here = page.url().await.unwrap_or_default().to_lowercase();
        // 登录页 / 扫码页：提示一次，然后继续等
        if here.contains("login") || here.contains("passport") || here.contains("sso") {
            if !told_login {
                println!("当前停在登录页：请在这个窗口里完成登录（扫码或账号密码），登录后自动继续。");
                told_login = true;
            }
            sleep(Duration::from_secs(1)).await;
            continue;
        }
        if !here.contains("login") && !here.contains("passport") {
            let media = eval_json(&page, "window.__YKT.snapshot()")
                .await
                .ok()
                .and_then(|s| s.get("found").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let listed = eval_json(&page, &format!("window.__YKT.entries({})", j(&start_sel)))
                .await
                .ok()
                .and_then(|v| v.as_array().map(|a| a.len()))
                .unwrap_or(0);
            if media || listed > 0 {
                break;
            }
        }
        sleep(Duration::from_secs(1)).await;
    }

    let mut finished = 0usize;
    let mut last_progress = Instant::now();
    let mut last_time = -1f64;
    let mut resume_ticks = 0u32;
    let auto_zoom = zoom <= 0.0;
    // 每点进一层就记下从哪页点的；这一层走完要回到上一层点下一个入口
    let mut trail: Vec<String> = Vec::new();
    // 当前视频是从哪个入口点进来的，完成时按这个 label 记账（快照标题不够稳）
    let mut current_entry: Option<String> = None;
    // 跳转后媒体要几秒才挂上，入口列表也要等渲染；没等够就判"这一级走完了"会来回弹
    let mut no_media = 0u32;
    let mut exhausted = 0u32;
    // 整页入口都被 attempts 上限挡住时，重置重试计数的次数（防止无限重来）
    let mut revive = 0u32;
    // 视频播完但站点还没标"已完成"时，刷新页面重新确认的次数
    let mut ended_ticks = 0u32;
    let mut refresh_tries = 0u32;
    // 连续点播放失败的次数：有些课根本没有可用媒体源，不能无限重试
    let mut play_fail = 0u32;
    // 播放中放慢节拍，其它状态（目录页、跳转中）用快节拍
    let mut tick_now = cfg.tick;

    loop {
        sleep(tick_now).await;
        if auto_zoom {
            let _ = page.fit_zoom().await;
        }

        let url = page.url().await.unwrap_or_default();
        let parsed: url::Url = match url.parse() {
            Ok(u) => u,
            Err(_) => continue,
        };
        let route = adapter.match_route(&parsed);
        let dismiss = route.map(|r| r.dismiss_selectors.clone()).unwrap_or_default();
        let entries_sel = route
            .map(|r| r.entry_selectors.clone())
            .unwrap_or_else(|| vec!["[class*=chapter] li".into(), "[class*=catalog] li".into()]);
        let threshold = route.map(|r| r.complete_pct).unwrap_or(99.0);
        let is_entry_page = matches!(route.map(|r| r.kind), Some(crate::adapter::RouteKind::Entry));

        let _ = eval_json(&page, &format!("window.__YKT.dismiss({})", j(dismiss))).await;

        let snap = match eval_json(&page, "window.__YKT.snapshot()").await {
            Ok(v) => v,
            Err(e) => {
                println!("取不到播放状态（可能还在跳转）：{e}");
                resume_ticks += 1;
                if resume_ticks > 20 {
                    println!("连续取不到状态，重新加载页面");
                    let _ = page.goto(&start_url).await;
                    resume_ticks = 0;
                }
                continue;
            }
        };
        resume_ticks = 0;

        if snap.get("found").and_then(|f| f.as_bool()) != Some(true) {
            last_time = -1.0;
            last_progress = Instant::now();
            no_media += 1;
            tick_now = cfg.tick;
            // 视频页常常是"先出外壳、点一下才挂 <video>"，所以要给它足够时间，
            // 并且主动点播放器区域把播放器唤出来。
            // 目录页本来就没有播放器，等太久只是白费时间，早点点下一个入口。
            let is_learn_page =
                matches!(route.map(|r| r.kind), Some(crate::adapter::RouteKind::Learn));
            let patience = if is_learn_page { 8 } else { 2 };
            if no_media <= patience {
                if is_learn_page {
                    let _ = eval_json(&page, &format!("window.__YKT.start({})", cfg.rate)).await;
                    if no_media == 1 {
                        println!("这一页还没有播放器，先点一下播放器区域，等它加载");
                    }
                }
                continue;
            }
            if finished >= cfg.max_items {
                println!("已处理 {finished} 项，达到上限，结束。");
                break;
            }
            match advance(&page, &mut store, &entries_sel, &state_path).await {
                Some(entry) => {
                    no_media = 0;
                    exhausted = 0;
                    // 只有「目录级」页面才记进回退栈；视频页里切下一集是同层动作，不用回退
                    if is_entry_page && trail.len() < 8 {
                        trail.push(url.clone());
                    }
                    current_entry = Some(entry.clone());
                    println!("进入下一项：{entry}");
                }
                None => {
                    exhausted += 1;
                    if exhausted == 1 {
                        continue;
                    }
                    // 刚进页面时课程列表常常还没渲染出来，误判成"没有入口"就会一路退出。
                    // 刷一次再看，给接口一点时间。
                    if exhausted == 2 {
                        println!("没找到可点的入口，刷新一次再看");
                        let _ = page.goto(&url).await;
                        sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                    if exhausted < 5 {
                        continue;
                    }
                    exhausted = 0;
                    no_media = 0;
                    if let Some(back_url) = trail.pop() {
                        println!("本级已走完，回到 {}", trim_url(&back_url));
                        let _ = page.goto(&back_url).await;
                        last_time = -1.0;
                        sleep(Duration::from_secs(3)).await;
                        continue;
                    }
                    // 连回退栈都空了还点不动：多半是每门的 attempts 都被跳过。
                    // 重置重试计数再跑一轮，避免误判成"已全部完成"。
                    if revive < 3 {
                        let n = store.reset_stalled(threshold);
                        if n > 0 {
                            revive += 1;
                            store.save(&state_path)?;
                            println!("入口都被试过上限挡住了，已重置 {n} 项的重试计数，从头再来一轮（第 {revive} 次）");
                            let _ = page.goto(&start_url).await;
                            last_time = -1.0;
                            sleep(Duration::from_secs(3)).await;
                            continue;
                        }
                    }
                    println!("没有可进入的学习项，可能已全部完成。");
                    println!("如果窗口里停的是首页/个人中心，请把「培训项目详情页」地址（列着全部课程、带「去学习」的那一页）填进设置再试。");
                    break;
                }
            }
            continue;
        }
        no_media = 0;
        exhausted = 0;
        // 已经在播了：放慢节拍，日志按秒级增量打就够了
        tick_now = Duration::from_secs(5);

        let cur = snap.get("currentTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let dur = snap.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let pct = snap.get("pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let paused = snap.get("paused").and_then(|v| v.as_bool()).unwrap_or(true);
        let ended = snap.get("ended").and_then(|v| v.as_bool()).unwrap_or(false);
        // 站点自己打的"已完成"才是准的：只看百分比会提前收工，服务端还没记完成。
        let site_done = snap.get("siteDone").and_then(|v| v.as_bool()).unwrap_or(false);
        let rate_now = snap.get("rate").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let label = snap.get("title").and_then(|v| v.as_str()).unwrap_or("当前项").to_string();

        // 站点要求"先点播放"才开播：直接调 play() 会被手势策略拒绝（NotAllowedError），
        // 所以统一走 start()——它先点播放器外壳、再驱动 media，倍速和静音一起落下。
        // 站点自己会在切集/弹提示时把倍速改回 1、把静音取消，所以每轮都校正。
        if paused || (rate_now - cfg.rate).abs() > 0.01 {
            let r = eval_json(&page, &format!("window.__YKT.start({})", cfg.rate)).await?;
            let started = r.get("started").and_then(|v| v.as_bool()).unwrap_or(false);
            let via = r.get("via").and_then(|v| v.as_str()).unwrap_or("?");
            let err = r.get("err").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if started {
                play_fail = 0;
            } else {
                play_fail += 1;
                println!(
                    "还没播起来（via={via} err={err}，第 {play_fail} 次），下一轮继续点播放"
                );
                // 有些课（没购买/没选课/资源下线）根本没有可用媒体源，
                // 再怎么点也是同一个错，死磕只会把整晚耗掉。
                let no_source = err.contains("no supported sources")
                    || err.contains("NotSupportedError")
                    || err.contains("empty-source");
                let limit = if no_source { 4 } else { 10 };
                if play_fail >= limit {
                    println!("这一项连续 {play_fail} 次播不起来（{err}），跳过它，稍后再回来补");
                    let key = current_entry.clone().unwrap_or_else(|| label.clone());
                    store.touch(&key, Status::Running, pct, true);
                    let _ = store.save(&state_path);
                    play_fail = 0;
                    match advance(&page, &mut store, &entries_sel, &state_path).await {
                        Some(entry) => {
                            current_entry = Some(entry.clone());
                            println!("继续：{entry}");
                        }
                        None => {
                            if let Some(back_url) = trail.pop() {
                                println!("这一页没有下一项了，回到 {}", trim_url(&back_url));
                                let _ = page.goto(&back_url).await;
                                sleep(Duration::from_secs(2)).await;
                            }
                        }
                    }
                    last_time = -1.0;
                    continue;
                }
            }
            last_progress = Instant::now();
        }

        // 完成判据只有一条：站点自己显示了"已完成"。
        // 视频播到结尾、进度到 99% 都不算——那时服务端往往还没记学时，
        // 提前切走就白看了。没等到标记就继续等，必要时刷新页面让它自己更新。
        if site_done {
            let key = current_entry.take().unwrap_or_else(|| label.clone());
            let why = "站点标记已完成";
            store.touch(&key, Status::Done, pct, false);
            store.save(&state_path)?;
            finished += 1;
            println!("完成 {key}（{why}，{pct:.1}%），累计 {finished} 项");
            if finished >= cfg.max_items {
                break;
            }
            match advance(&page, &mut store, &entries_sel, &state_path).await {
                Some(entry) => {
                    if is_entry_page && trail.len() < 8 {
                        trail.push(url.clone());
                    }
                    current_entry = Some(entry.clone());
                    println!("继续：{entry}");
                }
                None => {
                    if let Some(back_url) = trail.pop() {
                        println!("这一页学完了，回到 {}", trim_url(&back_url));
                        let _ = page.goto(&back_url).await;
                        sleep(Duration::from_secs(3)).await;
                    } else {
                        break;
                    }
                }
            }
            last_time = -1.0;
            continue;
        }

        // 视频已经播完，但站点还没打出"已完成"：先刷新让站点自己更新状态，
        // 连续几次都等不到就把这一项放回待重试（不算完成），免得死等一个视频。
        if ended {
            ended_ticks += 1;
            if ended_ticks >= 6 {
                ended_ticks = 0;
                refresh_tries += 1;
                if refresh_tries <= 2 {
                    println!("视频播完了，站点还没标已完成，刷新页面确认一次（第 {refresh_tries} 次）");
                    let _ = page.goto(&url).await;
                    sleep(Duration::from_secs(3)).await;
                    last_time = -1.0;
                    continue;
                }
                println!("刷新后仍没看到已完成标记，先跳到下一项，稍后再回来补");
                let key = current_entry.clone().unwrap_or_else(|| label.clone());
                store.touch(&key, Status::Running, pct, true);
                let _ = store.save(&state_path);
                refresh_tries = 0;
                match advance(&page, &mut store, &entries_sel, &state_path).await {
                    Some(entry) => {
                        current_entry = Some(entry.clone());
                        println!("继续：{entry}");
                    }
                    None => {
                        if let Some(back_url) = trail.pop() {
                            println!("这一页没有下一项了，回到 {}", trim_url(&back_url));
                            let _ = page.goto(&back_url).await;
                            sleep(Duration::from_secs(2)).await;
                        }
                    }
                }
                last_time = -1.0;
                continue;
            }
        } else {
            ended_ticks = 0;
        }

        if (cur - last_time).abs() > 0.5 {
            last_time = cur;
            last_progress = Instant::now();
            println!("{} {:.1}% ({:.0}/{:.0}s) x{rate_now}", label, pct, cur, dur);
        } else if last_progress.elapsed() > cfg.stall_limit {
            // 最小化时渲染进程会被冻住，进度不动不等于卡死：只补播，不判失败、不跳项
            println!("{label} 一段时间没动，补一次播放（最小化冻结属正常）");
            last_progress = Instant::now();
            let _ = eval_json(&page, &format!("window.__YKT.play({})", cfg.rate)).await;
        }
    }

    store.save(&state_path)?;
    session.close().await;
    Ok(())
}

async fn advance(
    page: &crate::browser::Page,
    store: &mut Store,
    selectors: &[String],
    state_path: &Path,
) -> Option<String> {
    let listed = eval_json(page, &format!("window.__YKT.entries({})", j(selectors))).await.ok()?;
    let items = listed.as_array()?;
    if items.is_empty() {
        return None;
    }
    for item in items {
        let label = item.get("label").and_then(|v| v.as_str())?.to_string();
        let done = item.get("done").and_then(|v| v.as_bool()).unwrap_or(false);
        if done {
            store.touch(&label, Status::Done, 100.0, false);
            continue;
        }
        // 正在播的那条不能再点，否则页面重载，永远停在"没媒体"
        if item.get("active").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        // 这里只信站点说的"已完成"（上面 done 分支）和本次运行的重试次数。
        // 本地 store 里的 Done 不能当依据：旧版本用 99% 误判记过一批，留着会永远跳过，
        // 表现就是"点进来 → 本级已走完 → 退回 → 换下一门"来回空转。
        if let Some(existing) = store.items.get(&label) {
            if existing.attempts >= 3 {
                continue;
            }
        }
        let idx = item.get("i").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let clicked = eval_json(
            page,
            &format!("window.__YKT.clickEntry({}, {idx})", j(selectors)),
        )
        .await
        .ok()?;
        if clicked.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            store.touch(&label, Status::Running, 0.0, true);
            let _ = store.save(state_path);
            return Some(label);
        }
    }
    None
}

fn j<T: serde::Serialize>(v: T) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "null".into())
}

/// 问一次服务端"当前是谁"，返回 yes / no / unknown。
async fn login_state(page: &crate::browser::Page) -> &'static str {
    if eval_json(page, LOGIN_PROBE_JS).await.is_err() {
        return "unknown";
    }
    for _ in 0..20 {
        sleep(Duration::from_millis(500)).await;
        let raw = match eval_json(page, "JSON.stringify(window.__YKT_LOGIN||{})").await {
            Ok(v) => v.as_str().unwrap_or("").to_string(),
            Err(_) => continue,
        };
        let state = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v.get("state").and_then(|x| x.as_str()).map(|s| s.to_string()))
            .unwrap_or_default();
        match state.as_str() {
            "yes" => return "yes",
            "no" => return "no",
            "unknown" => return "unknown",
            _ => continue,
        }
    }
    "unknown"
}

/// 确保处于登录态：未登录时把用户送回站点首页，等他登录完再继续。
/// 站点允许匿名看课，所以这一步不能省略——否则"登录后才刷课"这条前提就不成立了。
async fn ensure_login(page: &crate::browser::Page, origin: &str) {
    loop {
        match login_state(page).await {
            "yes" => {
                println!("已登录，开始刷课。");
                return;
            }
            "no" => {
                println!("检测到未登录：请在这个窗口里完成登录（扫码或账号密码），登录后会自动继续。");
                if !origin.is_empty() {
                    let _ = page.goto(&format!("{origin}/")).await;
                }
            }
            _ => {
                println!("无法确认登录状态（接口可能改版），按已登录继续。");
                return;
            }
        }
        // 等用户登录；期间不断复查
        let mut waited = 0u32;
        loop {
            sleep(Duration::from_secs(3)).await;
            waited += 1;
            if login_state(page).await == "yes" {
                println!("已登录，开始刷课。");
                return;
            }
            if waited % 20 == 0 {
                println!("仍在等待登录（已等 {} 秒）…", waited * 3);
            }
        }
    }
}

fn trim_url(url: &str) -> String {
    if url.len() > 70 {
        format!("{}…", &url[..70])
    } else {
        url.to_string()
    }
}

fn fnv(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

