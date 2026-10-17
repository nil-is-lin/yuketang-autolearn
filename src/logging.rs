//! 把 stdout/stderr 接管到主窗口里的日志面板。
//!
//! 为什么要绕到 pipe：代码里到处是 println!，逐个改成自定义宏既容易漏，
//! 也拿不到 anyhow 之类库自己打的东西。这里在进程启动最早期把 STD_OUTPUT/STD_ERROR
//! 换成匿名管道的写端，再起一个线程读管道：
//!   - 原样转发给真正的控制台（命令行运行时还能看到输出）；
//!   - 同时按行切分，投给界面线程往日志面板里追加。
//!
//! 必须在任何 println! 之前调用 install：Rust 的 stdout 句柄是首次使用时取
//! GetStdHandle 的结果，晚了就还是原来的控制台。

use std::io::Write;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, TRUE};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    GetConsoleProcessList, GetConsoleWindow, GetStdHandle, SetStdHandle, STD_ERROR_HANDLE,
    STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

static LOG_RX: OnceLock<Mutex<Option<Receiver<String>>>> = OnceLock::new();

/// 取走"日志行"的接收端。整个进程只该有界面线程来取。
pub fn take_rx() -> Option<Receiver<String>> {
    LOG_RX.get()?.lock().ok()?.take()
}

/// 接管 stdout/stderr。`hide_console` 为真时顺便把控制台窗口藏掉
/// （双击启动时就是这样：只留一个带日志面板的窗口）。
pub fn install(hide_console: bool) {
    let (tx, rx) = mpsc::channel::<String>();
    let _ = LOG_RX.set(Mutex::new(Some(rx)));

    match redirect(tx) {
        Ok(()) => {}
        Err(e) => {
            // GUI 子系统下本来就没有控制台，println! 写不出去会 panic；
            // 接管失败时把 stdout 接到 NUL，至少保证程序能跑。
            let _ = WriteFile; // 保留导入，避免 unused 警告干扰
            let _ = writeln!(std::io::stderr(), "日志面板接管失败：{e}");
            silence_stdio();
        }
    }

    if hide_console {
        unsafe {
            let hwnd = GetConsoleWindow();
            if !hwnd.is_invalid() {
                // 只有"这个控制台是我们自己独占的"才隐藏。
                // 从用户终端里启动时，控制台附着了一堆进程，藏掉等于藏了用户的终端。
                let mut pids = [0u32; 2];
                let owned = GetConsoleProcessList(&mut pids) <= 1;
                if owned {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        }
    }
}

fn redirect(tx: Sender<String>) -> anyhow::Result<()> {
    unsafe {
        let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
        // 句柄要可继承，WebView2 的子进程不会用到，但 CreatePipe 默认要求给安全属性
        CreatePipe(&mut read, &mut write, None, 0)?;

        let has_console = !GetConsoleWindow().is_invalid();
        let console = if has_console {
            GetStdHandle(STD_OUTPUT_HANDLE).ok()
        } else {
            None
        };
        let inherited_out = GetStdHandle(STD_OUTPUT_HANDLE).ok();
        let inherited_err = GetStdHandle(STD_ERROR_HANDLE).ok();

        SetStdHandle(STD_OUTPUT_HANDLE, write)?;
        SetStdHandle(STD_ERROR_HANDLE, write)?;

        // 没有控制台时（GUI 子系统、被脚本用管道启动），这些继承来的句柄留在我们手里，
        // 父进程会一直等管道 EOF，看起来就是"命令卡住不返回"。直接关掉。
        if !has_console {
            for h in [inherited_out, inherited_err].into_iter().flatten() {
                if !h.is_invalid() {
                    let _ = CloseHandle(h);
                }
            }
        }

        // HANDLE 里是裸指针，不能跨线程搬；转成整数再在闭包里还原
        let read_raw = read.0 as isize;
        let console_raw = console.map(|h| h.0 as isize);

        thread::Builder::new()
            .name("log-pipe".into())
            .spawn(move || {
                let read = HANDLE(read_raw as *mut std::ffi::c_void);
                let mut buf = [0u8; 4096];
                let mut pending: Vec<u8> = Vec::new();
                // 日志一律落盘：面板里的内容关窗就没了，出问题时要能回看。
                // 环境变量 YKT_LOG_FILE 可以换路径；默认写 state/logs/run-<时间>.log。
                let path = std::env::var("YKT_LOG_FILE").ok().unwrap_or_else(|| {
                    let dir = std::path::Path::new("state").join("logs");
                    let _ = std::fs::create_dir_all(&dir);
                    dir.join(format!(
                        "run-{}.log",
                        chrono::Local::now().format("%Y%m%d-%H%M%S")
                    ))
                    .to_string_lossy()
                    .to_string()
                });
                println!("日志文件：{path}");
                // 只留最近 10 份，避免长期运行把磁盘塞满
                if let Some(dir) = std::path::Path::new(&path).parent() {
                    if let Ok(rd) = std::fs::read_dir(dir) {
                        let mut files: Vec<_> = rd
                            .flatten()
                            .filter(|e| {
                                e.path().extension().and_then(|x| x.to_str()) == Some("log")
                            })
                            .collect();
                        if files.len() > 10 {
                            files.sort_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()));
                            for old in files.iter().take(files.len() - 10) {
                                let _ = std::fs::remove_file(old.path());
                            }
                        }
                    }
                }
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .ok();
                loop {
                    let mut got = 0u32;
                    if ReadFile(read, Some(&mut buf), Some(&mut got), None).is_err() {
                        break;
                    }
                    if got == 0 {
                        break;
                    }
                    let chunk = &buf[..got as usize];
                    if let Some(f) = file.as_mut() {
                        let _ = f.write_all(chunk);
                        let _ = f.flush();
                    }
                    // 仍然写给原来的控制台，命令行跑的时候照样有输出。
                    // 只回写 stdout 那一份：stderr 已经被接到同一个管道里了，
                    // 再把 stderr 句柄也写一遍，重定向场景下就会出现每行两遍。
                    if let Some(raw) = console_raw {
                        let h = HANDLE(raw as *mut std::ffi::c_void);
                        let mut wrote = 0u32;
                        let _ = WriteFile(h, Some(chunk), Some(&mut wrote), None);
                    }
                    pending.extend_from_slice(chunk);
                    while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=pos).collect();
                        let text = String::from_utf8_lossy(&line)
                            .trim_end_matches(['\r', '\n'])
                            .to_string();
                        if tx.send(text).is_err() {
                            return;
                        }
                    }
                }
            })?;
    }
    Ok(())
}

/// 把 stdout/stderr 接到 NUL，避免 GUI 子系统下 println! 因句柄无效而 panic。
fn silence_stdio() {
    unsafe {
        if let Ok(null) = windows::Win32::Storage::FileSystem::CreateFileW(
            windows::core::w!("NUL"),
            windows::Win32::Storage::FileSystem::FILE_GENERIC_WRITE.0,
            windows::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
            None,
            windows::Win32::Storage::FileSystem::OPEN_EXISTING,
            Default::default(),
            None,
        ) {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, null);
            let _ = SetStdHandle(STD_ERROR_HANDLE, null);
        }
    }
}

/// 只给日志面板用：把宽字符转成 PCWSTR 需要的 UTF-16 缓冲。
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 便于在界面线程里安全调用 SetWindowTextW
pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    PCWSTR(buf.as_ptr())
}

// TRUE 只是让编译器别把未使用的导入当成死代码（CreatePipe 的继承语义依赖它）
const _: windows::core::BOOL = TRUE;
