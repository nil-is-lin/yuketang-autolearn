//! WebView2 引擎：浏览器实例由本进程持有，不再通过端口去连一个外部 Edge。
//!
//! 换掉 CDP 的动机很具体：外部浏览器的调试端口要抢空闲口、会被已在跑的实例接管、
//! 连接还会被对端重置，这些都是"控制权不在我们手里"的产物。这里只有一个 STA 线程
//! 持有 ICoreWebView2，命令通过 channel 进去，结果用 oneshot 回来。

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::oneshot;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_WEB_ERROR_STATUS, CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2,
    ICoreWebView2Controller, ICoreWebView2Environment, ICoreWebView2EnvironmentOptions,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, CoTaskMemPWSTR,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    ExecuteScriptCompletedHandler, NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, WebMessageReceivedEventHandler,
};
use windows::core::{BOOL, PCWSTR, PWSTR, w};
use windows::Win32::Foundation::{E_POINTER, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DEFAULT_GUI_FONT, EndPaint, FillRect, GRAY_BRUSH, GetStockObject, HBRUSH,
    InvalidateRect, PAINTSTRUCT, ScreenToClient, UpdateWindow,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{EM_SCROLLCARET, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetCursorPos, GetParent, MSG,
    MoveWindow, PM_REMOVE, PostQuitMessage, PeekMessageW, RegisterClassW, SendMessageW, SetCursor,
    SetProcessDPIAware, ShowWindow, TranslateMessage, WNDCLASSW, CW_USEDEFAULT, ES_AUTOVSCROLL,
    ES_MULTILINE, ES_READONLY, HICON, IDC_SIZENS, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED,
    LoadCursorW, LoadImageW, SW_SHOWMAXIMIZED, WM_CLOSE, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_PAINT, WM_QUIT, WM_SETFONT, WM_SETCURSOR, WM_SIZE, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE, WS_VSCROLL,
};

const EVAL_TIMEOUT: Duration = Duration::from_secs(90);
/// 导航后最多等这么久；站点自己的 SPA 跳转常常让 NavigationCompleted 迟迟不来。
/// 等太久会把整个消息循环卡住（表现为点了入口半天没反应），8 秒足够真实导航完成。
const NAV_TIMEOUT: Duration = Duration::from_secs(8);

/// STA 线程收到的事件转成的请求。
enum Req {
    Eval {
        js: String,
        reply: oneshot::Sender<Result<String>>,
    },
    InitScript {
        js: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Navigate {
        url: String,
        reply: oneshot::Sender<Result<()>>,
    },
    /// 用页面内 HTML 当界面（设置页就是这么做出来的，省得引 GUI 框架）
    Html {
        html: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Zoom(f64),
    Quit,
}

/// 发给 STA 线程的句柄，可以克隆。
#[derive(Clone)]
pub struct Web {
    tx: Sender<Req>,
    /// 当前缩放系数，所有克隆共享一份；自适应缩放要拿它做基准
    zoom: Arc<AtomicU64>,
    /// 页面 postMessage 过来的消息（设置页提交就是走这条路）
    inbox: Arc<Mutex<Vec<String>>>,
}

impl Web {
    pub fn zoom(&self) -> f64 {
        f64::from_bits(self.zoom.load(Ordering::Relaxed))
    }

    pub fn set_zoom(&self, value: f64) {
        let value = value.clamp(0.3, 2.0);
        self.zoom.store(value.to_bits(), Ordering::Relaxed);
        let _ = self.tx.send(Req::Zoom(value));
    }

    /// 把页面排版宽度缩进可视区。课程表列多，写死一个系数总有不合适的屏幕。
    ///
    /// 判据只认"要点的那些按钮有没有被裁到视口外"：文档 scrollWidth 恒等于视口宽
    /// （表格是压扁自适应的，不会撑出滚动条），而全量元素的右边界又会被轮播、悬浮面板带偏，
    /// 只有按钮被切掉才是真的"显示不全"。
    ///
    /// 只在超出时缩小，不放大：缩到放得下之后右边界回到视口内，不会再抖。
    pub async fn fit_zoom(&self) -> Result<f64> {
        let v = self
            .eval_json_string(
                "(()=>{let need=0;const els=document.querySelectorAll('button,a,[role=button]');\
                 const n=Math.min(els.length,1200);\
                 for(let i=0;i<n;i++){const e=els[i];const t=(e.innerText||'').trim();\
                 if(!t||t.length>12)continue;const r=e.getBoundingClientRect();\
                 if(r.width<2||r.height<2)continue;if(r.right>need)need=r.right;}\
                 return JSON.stringify({iw:document.documentElement.clientWidth,need:need});})()",
            )
            .await?;
        let iw = v.get("iw").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let need = v.get("need").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let cur = self.zoom();
        // 窗口被最小化时 innerWidth 会缩到几百，这时候量出来的"超出"是假的；
        // 留 4px 余量，否则贴边的按钮会被当成裁切、一路缩到下限
        if iw < 700.0 || need <= iw + 4.0 {
            return Ok(cur);
        }
        let fitted = (cur * iw / (need + 8.0)).clamp(0.55, 2.0);
        if (fitted - cur).abs() > 0.01 {
            self.set_zoom(fitted);
        }
        Ok(fitted)
    }
}

pub struct Session {
    web: Web,
    worker: Option<JoinHandle<()>>,
}

impl Session {
    /// profile 目录就是 WebView2 的 userDataFolder，登录态存在里面，和系统 Edge 的档案互不影响。
    /// browser_folder 只在机器上没有 WebView2 运行时时需要填，指向某个 Edge 的安装目录。
    /// zoom 传 0 表示先按 1.0 起屏，之后由调用方 fit_zoom 自适应。
    pub fn spawn(
        profile: &PathBuf,
        browser_folder: Option<&str>,
        window: (u32, u32),
        zoom: f64,
    ) -> Result<Session> {
        let (tx, rx) = mpsc::channel();
        let initial = if zoom > 0.0 { zoom.clamp(0.3, 2.0) } else { 1.0 };
        let zoom_cell = Arc::new(AtomicU64::new(initial.to_bits()));
        let inbox: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let inbox_worker = inbox.clone();
        let profile = profile.clone();
        let browser_folder = browser_folder.map(|s| s.to_string());
        let worker = thread::Builder::new()
            .name("webview2-sta".into())
            .spawn(move || {
                if let Err(e) = run(
                    rx,
                    &profile,
                    browser_folder.as_deref(),
                    window,
                    initial,
                    inbox_worker,
                ) {
                    eprintln!("WebView2 线程退出：{e:#}");
                }
            })
            .map_err(|e| anyhow!("启动 WebView2 线程失败：{e}"))?;
        Ok(Session {
            web: Web {
                tx,
                zoom: zoom_cell,
                inbox,
            },
            worker: Some(worker),
        })
    }

    /// 拿到页面句柄但不导航：需要在首屏脚本之前挂钩子时用这条。
    pub fn page(&self) -> Web {
        self.web.clone()
    }

    pub async fn navigate(&self, url: &str) -> Result<Web> {
        let page = self.web.clone();
        page.goto(url).await?;
        Ok(page)
    }

    pub async fn close(mut self) {
        let _ = self.web.tx.send(Req::Quit);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Web {
    pub async fn eval(&self, js: &str) -> Result<Value> {
        let raw = self.raw(js).await?;
        // ExecuteScript 回的是 JSON 文本；表达式没有返回值时是 "null"
        if raw.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&raw)?)
    }

    /// 求值一段返回 JSON 字符串的脚本，再把字符串本身解析掉
    pub async fn eval_json_string(&self, js: &str) -> Result<Value> {
        match self.eval(js).await? {
            Value::String(s) => Ok(serde_json::from_str(&s).unwrap_or(Value::String(s))),
            other => Ok(other),
        }
    }

    pub async fn url(&self) -> Result<String> {
        Ok(self
            .eval("location.href")
            .await?
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    pub async fn goto(&self, url: &str) -> Result<()> {
        self.request(|reply| Req::Navigate {
            url: url.to_string(),
            reply,
        })
        .await
    }

    /// 用一段 HTML 当界面（设置页）。不需要本地文件、也不需要起 http 服务。
    pub async fn show_html(&self, html: &str) -> Result<()> {
        self.request(|reply| Req::Html {
            html: html.to_string(),
            reply,
        })
        .await
    }

    /// 取出页面用 postMessage 发过来的消息（JSON 文本）。设置页提交走这条路。
    pub fn take_message(&self) -> Option<String> {
        self.inbox.lock().ok().and_then(|mut q| {
            if q.is_empty() {
                None
            } else {
                Some(q.remove(0))
            }
        })
    }

    pub async fn add_init_script(&self, js: &str) -> Result<()> {
        self.request(|reply| Req::InitScript {
            js: js.to_string(),
            reply,
        })
        .await
    }

    async fn raw(&self, js: &str) -> Result<String> {
        let js = js.to_string();
        self.request(move |reply| Req::Eval { js, reply }).await
    }

    /// 把请求交给 STA 线程，等回包；这条 await 在 tokio 上，不阻塞消息泵
    async fn request<T: Send + 'static>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T>>) -> Req,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .map_err(|_| anyhow!("WebView2 线程已经不在了"))?;
        tokio::time::timeout(EVAL_TIMEOUT, rx)
            .await
            .map_err(|_| anyhow!("等 WebView2 回包超时（{:?}）", EVAL_TIMEOUT))?
            .map_err(|_| anyhow!("WebView2 线程没有回包（多半是窗口被关掉了）"))?
    }
}

thread_local! {
    static CONTROLLER: RefCell<Option<ICoreWebView2Controller>> = const { RefCell::new(None) };
    /// 窗口下半部分的日志面板句柄
    static LOG_EDIT: Cell<isize> = const { Cell::new(0) };
    /// 分隔条句柄
    static SPLITTER: Cell<isize> = const { Cell::new(0) };
    static DRAGGING: Cell<bool> = const { Cell::new(false) };
}

/// 日志面板高度（像素），拖动分隔条会改它。播放器占上面，日志占下面。
static LOG_H_PX: AtomicI32 = AtomicI32::new(190);
/// 分隔条厚度
const SPLIT_H: i32 = 6;

/// 按当前客户区重新排布：上面 WebView2，中间分隔条，下面日志面板。
fn layout(hwnd: HWND) {
    unsafe {
        let mut rect = RECT::default();
        if GetClientRect(hwnd, &mut rect).is_err() {
            return;
        }
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        let log_h = LOG_H_PX.load(Ordering::Relaxed).clamp(60, (h - 220).max(60));
        let web_h = (h - log_h - SPLIT_H).max(120);
        CONTROLLER.with(|c| {
            if let Some(controller) = c.borrow().as_ref() {
                let _ = controller.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: web_h,
                });
            }
        });
        let split = SPLITTER.with(|c| c.get());
        if split != 0 {
            let _ = MoveWindow(HWND(split as *mut _), 0, web_h, w, SPLIT_H, true);
        }
        let edit = LOG_EDIT.with(|c| c.get());
        if edit != 0 {
            let _ = MoveWindow(
                HWND(edit as *mut _),
                0,
                web_h + SPLIT_H,
                w,
                h - web_h - SPLIT_H,
                true,
            );
        }
        // 调整完必须强制重绘：WebView2 的合成层会把旧画面留在下半屏，
        // 看起来就像"日志面板没出现"。
        let _ = InvalidateRect(Some(hwnd), None, true);
        let _ = UpdateWindow(hwnd);
    }
}

/// 分隔条：拖动它改日志面板高度。用独立子窗口而不是在父窗口里算命中，
/// 这样鼠标形状、拖动捕获都由系统按窗口边界处理，简单可靠。
unsafe extern "system" fn splitter_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            if let Ok(cur) = LoadCursorW(None, IDC_SIZENS) {
                SetCursor(Some(cur));
            }
            return LRESULT(1);
        }
        WM_LBUTTONDOWN => {
            DRAGGING.with(|d| d.set(true));
            SetCapture(hwnd);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            DRAGGING.with(|d| d.set(false));
            let _ = ReleaseCapture();
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let dragging = DRAGGING.with(|d| d.get());
            if dragging {
                if let Ok(parent) = GetParent(hwnd) {
                    let mut pt = POINT::default();
                    if GetCursorPos(&mut pt).is_ok() {
                        let _ = ScreenToClient(parent, &mut pt);
                        let mut r = RECT::default();
                        if GetClientRect(parent, &mut r).is_ok() {
                            // 日志面板高度 = 客户区底 − 鼠标位置；上下都留出最小可用空间
                            let want = (r.bottom - pt.y).clamp(60, (r.bottom - 220).max(60));
                            LOG_H_PX.store(want, Ordering::Relaxed);
                            layout(parent);
                        }
                    }
                }
            }
            return LRESULT(0);
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            let brush = HBRUSH(GetStockObject(GRAY_BRUSH).0);
            let _ = FillRect(hdc, &r, brush);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_SIZE {
        layout(hwnd);
        return LRESULT(0);
    }
    if msg == WM_CLOSE {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn run(
    rx: Receiver<Req>,
    profile: &PathBuf,
    browser_folder: Option<&str>,
    window: (u32, u32),
    zoom: f64,
    inbox: Arc<Mutex<Vec<String>>>,
) -> Result<()> {
    let hwnd = unsafe {
        // 已经初始化过 COM 的线程会返回 RPC_E_CHANGED_MODE，这里不需要区分
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _ = SetProcessDPIAware();

        let instance = GetModuleHandleW(None).map_err(|e| anyhow!("取模块句柄失败：{e}"))?;
        // 窗口图标直接用 exe 里埋的那份（资源 id 1），标题栏和任务栏才不会显示默认图标
        let icon = LoadImageW(
            Some(instance.into()),
            PCWSTR(1 as *const u16),
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        )
        .unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            lpszClassName: w!("YuketangAutolearnHost"),
            hInstance: instance.into(),
            hIcon: HICON(icon.0),
            ..Default::default()
        };
        RegisterClassW(&class);

        // 分隔条用独立窗口类：拖动改上下比例，鼠标移到上面变上下箭头
        let split_class = WNDCLASSW {
            lpfnWndProc: Some(splitter_proc),
            lpszClassName: w!("YuketangAutolearnSplit"),
            hInstance: instance.into(),
            ..Default::default()
        };
        RegisterClassW(&split_class);

        let hwnd = CreateWindowExW(
            Default::default(),
            w!("YuketangAutolearnHost"),
            w!("雨课堂自动学习"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            window.0 as i32,
            window.1 as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| anyhow!("建窗口失败：{e}"))?;
        let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);
        hwnd
    };

    // 下半部分放日志面板：只留一个窗口，日志和播放器一起看。
    unsafe {
        let edit = CreateWindowExW(
            Default::default(),
            w!("EDIT"),
            // 初始就给一段文字：一眼能看出这个面板在不在
            w!("[日志] 启动中…"),
            WS_CHILD
                | WS_VISIBLE
                | WS_VSCROLL
                | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
                    (ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY) as u32,
                ),
            0,
            0,
            100,
            60,
            Some(hwnd),
            None,
            None,
            None,
        )?;
        let font = GetStockObject(DEFAULT_GUI_FONT);
        SendMessageW(
            edit,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        LOG_EDIT.with(|c| c.set(edit.0 as isize));
        // 分隔条：拖动它就能调上下比例，不用改代码
        let split = CreateWindowExW(
            Default::default(),
            w!("YuketangAutolearnSplit"),
            w!(""),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            100,
            SPLIT_H,
            Some(hwnd),
            None,
            None,
            None,
        )?;
        SPLITTER.with(|c| c.set(split.0 as isize));
        layout(hwnd);
    }

    // 日志行由界面线程从这里取；take_rx 只能成功一次
    let mut log_rx = crate::logging::take_rx();
    let mut log_text = String::new();
    println!(
        "日志面板接管：{}",
        if log_rx.is_some() { "已接通" } else { "没拿到管道（只在控制台输出）" }
    );

    let environment = create_environment(profile, browser_folder)?;
    let controller = unsafe {
        let (tx, done) = mpsc::channel();
        CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| {
                environment
                    .CreateCoreWebView2Controller(hwnd, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |error_code, controller| {
                error_code?;
                let _ = tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                Ok(())
            }),
        )
        .map_err(|e| anyhow!("创建控制器失败：{e}"))?;
        let controller = rx_none(done.recv())??;
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        controller.SetBounds(rect)?;
        controller.SetIsVisible(true)?;
        CONTROLLER.with(|c| *c.borrow_mut() = Some(controller.clone()));
        layout(hwnd);
        controller
    };
    let webview = unsafe { controller.CoreWebView2()? };
    // 课程表列很多，1360 宽的窗口装不下；缩放系数由 --zoom 给
    if (0.3..=3.0).contains(&zoom) {
        unsafe {
            controller.SetZoomFactor(zoom)?;
        }
    }

    // 课程页常用 window.open 打开学习页；不接住这个事件的话，WebView2 直接把弹窗丢掉，
    // 表现就是"点了没反应"。
    //
    // 关键：不能在回调线程里直接 Navigate。这个回调跑在 WebView2 的浏览器事件线程上，
    // 在它内部同步导航会让新文档停在一张白纸上（实测点进视频页后 body 一直是空的，
    // 连续 100 多秒都取不到任何元素）。所以这里只把地址塞进队列，由主消息循环去导航。
    let mut new_window_token = 0i64;
    let pending_popup: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let popup_slot = pending_popup.clone();
    let popup_handler = NewWindowRequestedEventHandler::create(Box::new(move |_sender, args| {
        let Some(args) = args else { return Ok(()) };
        let mut uri = PWSTR::null();
        unsafe {
            args.Uri(&mut uri)?;
            let target = uri.to_string().unwrap_or_default();
            let _ = args.SetHandled(true);
            if !target.is_empty() {
                println!("[新窗口] 收到 {target}");
                if let Ok(mut slot) = popup_slot.lock() {
                    *slot = Some(target);
                }
            }
        }
        Ok(())
    }));
    let mut new_window_token = 0i64;
    unsafe {
        webview.add_NewWindowRequested(&popup_handler, &mut new_window_token)?;
    }

    // 页面里的 postMessage：设置页点「开始刷课」就是靠它把地址和倍速传回来。
    // 这里只负责收，解析交给 tokio 侧（它更清楚接下来该干什么）。
    let mut msg_token = 0i64;
    let msg_inbox = inbox.clone();
    let msg_handler = WebMessageReceivedEventHandler::create(Box::new(move |_sender, args| {
        if let Some(args) = args {
            let mut text = PWSTR::null();
            unsafe {
                let _ = args.WebMessageAsJson(&mut text);
            }
            let raw = unsafe { text.to_string() }.unwrap_or_default();
            if !raw.is_empty() {
                // 消息内容记一笔：设置页"自己提交"这类怪事全靠它定位
                println!("[页面消息] {}", raw.chars().take(200).collect::<String>());
                if let Ok(mut q) = msg_inbox.lock() {
                    q.push(raw);
                }
            }
        }
        Ok(())
    }));
    unsafe {
        webview.add_WebMessageReceived(&msg_handler, &mut msg_token)?;
    }

    // 导航事件日志：页面"看着进去了却一直求不到值"时，只有看这两个事件才知道
    // 是导航没完成、还是被重定向到登录/错误页。
    let mut nav_start_token = 0i64;
    let nav_start = NavigationStartingEventHandler::create(Box::new(|_sender, args| {
        if let Some(args) = args {
            let mut uri = PWSTR::null();
            let text = unsafe {
                let _ = args.Uri(&mut uri);
                uri.to_string().unwrap_or_default()
            };
            // 内置页面是 data: URL，整段打出来会把日志刷爆
            if text.starts_with("data:") {
                println!("[导航开始] 内置页面（设置页）");
            } else {
                println!("[导航开始] {text}");
            }
        }
        Ok(())
    }));
    unsafe {
        webview.add_NavigationStarting(&nav_start, &mut nav_start_token)?;
    }

    let mut nav_done_token = 0i64;
    let nav_done = NavigationCompletedEventHandler::create(Box::new(|_sender, args| {
        if let Some(args) = args {
            let mut ok = BOOL::default();
            let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
            unsafe {
                let _ = args.IsSuccess(&mut ok);
                let _ = args.WebErrorStatus(&mut status);
            }
            println!("[导航完成] success={} status={}", ok.as_bool(), status.0);
        }
        Ok(())
    }));
    unsafe {
        webview.add_NavigationCompleted(&nav_done, &mut nav_done_token)?;
    }

    // 站点校验来路：宿主直接 Navigate 时浏览器不带 Referer，服务端会判成"非法进入"，
    // 页面停在空白（实测 html 长度一直是 0）。这里放开 referrer，让导航带上来源页。
    let _ = allow_referrer(&webview);

    // 每个新文档都装两段：反检测（可见性/焦点/鼠标/静音）+ 弹窗就地化。
    // 弹窗交给站点自己的 JS 跳转，新文档才拿得到 opener/referrer；
    // 由宿主 Navigate 过去会被判成"非法进入"，停在空白页或者弹"请先购买课程"。
    // 设 YKT_NO_GUARD=1 可关掉这两段，用来隔离"是不是注入脚本本身把页面弄坏了"。
    if std::env::var("YKT_NO_GUARD").as_deref() != Ok("1") {
        let guard = String::from(include_str!("../assets/anti-detect.js"));
        let nav = String::from(include_str!("../assets/nav-hook.js"));
        let boot = format!("{nav}\n{guard}");
        let _ = add_init_script(&webview, &boot);
        let _ = execute_script(&webview, &boot);
    } else {
        println!("已按 YKT_NO_GUARD=1 跳过注入脚本");
    }

    loop {
        // try_recv 会真的把请求取走，一轮里只准调一次，否则下一条命令会被吞掉、
        // 调用方等到的是"没有回包"。
        let mut busy = false;
        match rx.try_recv() {
            Ok(Req::Quit) => break,
            Ok(req) => {
                handle(req, &webview);
                busy = true;
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        let mut quit = false;
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    quit = true;
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                busy = true;
            }
        }
        if quit {
            break;
        }
        // 弹窗地址由这里导航：回调线程里同步 Navigate 会让新文档白屏
        if let Ok(mut slot) = pending_popup.lock() {
            if let Some(target) = slot.take() {
                println!("[新窗口] 就地打开 {target}");
                let _ = navigate(&webview, &target);
                busy = true;
            }
        }
        // 把管道里攒下的日志行贴到面板末尾
        if let Some(rx) = log_rx.as_ref() {
            let mut got = false;
            while let Ok(line) = rx.try_recv() {
                log_text.push_str(&line);
                log_text.push_str("\r\n");
                got = true;
            }
            if got {
                // 只留最后若干行，避免长时间运行把控件撑爆
                const KEEP: usize = 4000;
                if log_text.len() > KEEP * 4 {
                    let cut = log_text.len() - KEEP * 2;
                    let cut = log_text
                        .char_indices()
                        .map(|(i, _)| i)
                        .find(|i| *i >= cut)
                        .unwrap_or(log_text.len());
                    log_text = log_text[cut..].to_string();
                }
                let wide = crate::logging::wide(&log_text);
                unsafe {
                    SendMessageW(
                        HWND(LOG_EDIT.with(|c| c.get()) as *mut _),
                        // WM_SETTEXT = 0x000C，用 SendMessageW 直接设整段文本
                        0x000C,
                        None,
                        Some(LPARAM(wide.as_ptr() as isize)),
                    );
                    // 滚到末尾
                    let edit = HWND(LOG_EDIT.with(|c| c.get()) as *mut _);
                    let len = log_text.chars().count() as isize;
                    SendMessageW(edit, EM_SETSEL, Some(WPARAM(len as usize)), Some(LPARAM(len)));
                    SendMessageW(edit, EM_SCROLLCARET, None, None);
                }
                busy = true;
            }
        }
        if !busy {
            thread::sleep(Duration::from_millis(15));
        }
    }

    CONTROLLER.with(|c| c.borrow_mut().take());
    unsafe {
        let _ = controller.Close();
    }
    Ok(())
}

fn rx_none<T>(v: std::result::Result<T, mpsc::RecvError>) -> Result<T> {
    v.map_err(|e| anyhow!("WebView2 初始化被打断：{e}"))
}

/// 同步装一段"每个新文档都跑"的脚本。
fn add_init_script(webview: &ICoreWebView2, js: &str) -> Result<()> {
    let js = js.to_string();
    let webview = webview.clone();
    AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            let script = CoTaskMemPWSTR::from(js.as_str());
            unsafe {
                webview
                    .AddScriptToExecuteOnDocumentCreated(*script.as_ref().as_pcwstr(), &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }
        }),
        Box::new(|error_code, _id| error_code),
    )
    .map(|_| ())
    .map_err(|e| anyhow!("初始化脚本没生效：{e}"))
}

/// 同步在"当前文档"里求值一次，不关心返回值。
fn execute_script(webview: &ICoreWebView2, js: &str) -> Result<()> {
    let js = js.to_string();
    let webview = webview.clone();
    ExecuteScriptCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            let script = CoTaskMemPWSTR::from(js.as_str());
            unsafe {
                webview
                    .ExecuteScript(*script.as_ref().as_pcwstr(), &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }
        }),
        Box::new(|error_code, _text| error_code),
    )
    .map(|_| ())
    .map_err(|e| anyhow!("执行脚本失败：{e}"))
}

fn create_environment(profile: &PathBuf, browser_folder: Option<&str>) -> Result<ICoreWebView2Environment> {
    let folder = profile.display().to_string();
    let exe_folder = browser_folder.map(|s| s.to_string());
    let (tx, rx) = mpsc::channel();

    // 启动参数：站点会按"是不是我们自己打开的页面、有没有被判定为后台"来暂停播放。
    // 这里关掉后台节流和媒体自动播放限制，并放开 referrer 传递。
    let args = String::from(
        "--disable-features=CalculateNativeWinOcclusion,IntensiveWakeUpThrottling \
         --disable-background-timer-throttling \
         --disable-backgrounding-occluded-windows \
         --disable-renderer-backgrounding \
         --autoplay-policy=no-user-gesture-required \
         --disable-blink-features=AutomationControlled",
    );
    let options: ICoreWebView2EnvironmentOptions =
        webview2_com::CoreWebView2EnvironmentOptions::default().into();
    unsafe {
        let a = CoTaskMemPWSTR::from(args.as_str());
        let _ = options.SetAdditionalBrowserArguments(*a.as_ref().as_pcwstr());
    }

    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            let folder = CoTaskMemPWSTR::from(folder.as_str());
            let exe = exe_folder.as_deref().map(CoTaskMemPWSTR::from);
            let exe_pcwstr = match &exe {
                Some(e) => *e.as_ref().as_pcwstr(),
                None => PCWSTR::null(),
            };
            unsafe {
                CreateCoreWebView2EnvironmentWithOptions(
                    exe_pcwstr,
                    *folder.as_ref().as_pcwstr(),
                    Some(&options),
                    &handler,
                )
                .map_err(webview2_com::Error::WindowsError)
            }
        }),
        Box::new(move |error_code, environment| {
            error_code?;
            let _ = tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)));
            Ok(())
        }),
    )
    .map_err(|e| anyhow!("启动 WebView2 环境失败：{e}"))?;
    rx_none(rx.recv())?
        .map_err(|e| anyhow!("创建 WebView2 环境失败：{e}"))
}

/// 放开 referrer，让后续导航带上来源页（站点靠它判断是否从课程页进来的）。
fn allow_referrer(webview: &ICoreWebView2) -> Result<()> {
    let js = "try{const m=document.createElement('meta');m.name='referrer';m.content='unsafe-url';\
              (document.head||document.documentElement).appendChild(m);}catch(e){}";
    add_init_script(webview, js)
}

fn handle(req: Req, webview: &ICoreWebView2) {
    // handler 的闭包要求 'static，COM 指针本身是引用计数句柄，克隆一份即可
    let webview = webview.clone();
    match req {
        Req::Eval { js, reply } => {
            // 关键：这里不能同步等结果。ExecuteScript 的回调要由本线程的消息泵派发，
            // 而 wait_for_async_operation 内部自己也在泵消息，两边互相套住就死锁；
            // 表现就是"页面明明进去了，却一直求不到值"。
            // 所以只发出请求，结果由回调直接送进 tokio 那边的 oneshot。
            let script = CoTaskMemPWSTR::from(js.as_str());
            let handler = match ExecuteScriptCompletedHandler::create(Box::new(move |code, text| {
                let payload = match code {
                    Ok(()) => Ok(text),
                    Err(e) => Err(anyhow!("ExecuteScript 失败：{e}")),
                };
                let _ = reply.send(payload);
                Ok(())
            })) {
                handler => handler,
            };
            unsafe {
                if let Err(e) = webview.ExecuteScript(*script.as_ref().as_pcwstr(), &handler) {
                    // 发起就失败时仍要给调用方一个答复，否则它会等到超时
                    let _ = handler;
                    println!("ExecuteScript 发起失败：{e}");
                }
            }
        }
        Req::InitScript { js, reply } => {
            let result = AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
                Box::new(move |handler| {
                    let script = CoTaskMemPWSTR::from(js.as_str());
                    unsafe {
                        webview
                            .AddScriptToExecuteOnDocumentCreated(
                                *script.as_ref().as_pcwstr(),
                                &handler,
                            )
                            .map_err(webview2_com::Error::WindowsError)
                    }
                }),
                Box::new(|error_code, _id| error_code),
            )
            .map(|_| ())
            .map_err(|e| anyhow!("初始化脚本没生效：{e}"));
            let _ = reply.send(result);
        }
        Req::Navigate { url, reply } => {
            let result = navigate(&webview, &url);
            let _ = reply.send(result);
        }
        Req::Html { html, reply } => {
            let text = CoTaskMemPWSTR::from(html.as_str());
            let result = unsafe {
                webview
                    .NavigateToString(*text.as_ref().as_pcwstr())
                    .map_err(|e| anyhow!("打开内置页面失败：{e}"))
            };
            let _ = reply.send(result);
        }
        Req::Zoom(value) => {
            CONTROLLER.with(|c| {
                if let Some(controller) = c.borrow().as_ref() {
                    unsafe {
                        let _ = controller.SetZoomFactor(value);
                    }
                }
            });
        }
        Req::Quit => {}
    }
}

fn navigate(webview: &ICoreWebView2, url: &str) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let handler = NavigationCompletedEventHandler::create(Box::new(move |_sender, args| {
        let done = match args {
            Some(args) => {
                let mut issuccess = BOOL::default();
                unsafe { args.IsSuccess(&mut issuccess)? };
                let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                unsafe {
                    let _ = args.WebErrorStatus(&mut status);
                }
                (issuccess.as_bool(), status.0)
            }
            None => (false, -1),
        };
        let _ = tx.send(done);
        Ok(())
    }));
    let mut token = 0i64;
    unsafe {
        webview.add_NavigationCompleted(&handler, &mut token)?;
        let target = CoTaskMemPWSTR::from(url);
        let started = webview.Navigate(*target.as_ref().as_pcwstr());
        if let Err(e) = started {
            let _ = webview.remove_NavigationCompleted(token);
            return Err(anyhow!("导航 {url} 发起失败：{e}"));
        }
    }
    // 站点在拿到新页面后会立刻做一次 SPA 跳转，NavigationCompleted 可能根本等不到；
    // 这里只等一个上限，不把等不到当成失败——页面是否真的可用由调用方求值判断。
    let outcome = pump_until(&rx, NAV_TIMEOUT);
    let result = match outcome {
        Some((true, _)) => Ok(()),
        Some((false, code)) => Err(anyhow!(
            "导航 {} 被报为未完成，WebView2 错误码 {code}（详见 COREWEBVIEW2_WEB_ERROR_STATUS）",
            trim_url(url)
        )),
        None => Ok(()),
    };
    unsafe {
        let _ = webview.remove_NavigationCompleted(token);
    }
    result
}

/// 在消息泵里等到收到结果或超时，超时返回 None。
fn pump_until<T>(rx: &Receiver<T>, limit: Duration) -> Option<T> {
    let deadline = std::time::Instant::now() + limit;
    let mut msg = MSG::default();
    loop {
        if let Ok(v) = rx.try_recv() {
            return Some(v);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn trim_url(url: &str) -> String {
    if url.len() > 70 {
        format!("{}…", &url[..70])
    } else {
        url.to_string()
    }
}
