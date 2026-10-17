# 雨课堂自动刷课（yuketang-autolearn）

一个 Rust 写的 Windows 单文件 exe：内嵌 WebView2 打开真实页面，登录一次后自动
按「培训项目 → 课程 → 章节 → 视频」逐级点进去，自动点播放、2 倍速、静音，
学完一项自动切下一项。

**界面只有一个窗口**：上半部分是播放器，下半部分是实时日志（编译成 Windows GUI
子系统，双击运行不会弹出黑色控制台窗口，exe 也带了自己的图标）。中间的灰色分隔条
可以直接拖动，调整上下比例。

## 免责声明

本项目仅供**个人学习与自动化技术研究**，用来研究 WebView2、Win32 窗口合成、
浏览器注入这些技术点。使用前请自行确认：

- 平台的服务条款是否允许自动化播放；很多平台明确禁止，**风险自负**；
- 不要用它代替真正的学习——是否算完成学时、能否拿到证书，最终由平台判定；
- 不要用于商业用途、代刷、多账号批量操作。

作者不对因使用本工具导致的任何后果负责，包括但不限于账号受限、学时作废。

## 快速开始

1. 下载/编译 `yuketang-autolearn.exe`；
2. 第一次运行必须给一个「培训项目详情页」地址（就是列着全部课程、带「去学习」按钮的那页）：

```powershell
.\yuketang-autolearn.exe learn --url https://<你的学校域名>/pro/trainingproject/mytraining/detail/<id>
```

跑过一次会记在 `state/last_url.txt`，之后双击 exe 就沿用上次的地址。

常用参数：

| 参数 | 说明 |
| --- | --- |
| `--rate 2.0` | 倍速，默认 2.0（平台播放器上限；再高有被清空进度的风险） |
| `--max N` | 这次最多刷完几项，默认 0 = 刷到没有下一项为止 |
| `--keep-login` | 保留登录态，下次启动免登录（默认**不**保留） |
| `--switch` | 清空登录态，强制重新登录 |
| `--zoom 0.8` | 固定页面缩放；不填=自动适配窗口宽度 |
| `--url ...` | 指定起点 |

> 没有内置任何学校地址：仓库是公共的，把某个学校的内网入口写死既不通用也泄露信息。

## 登录与安全

- **默认不留登录态**：每次启动都用一次性会话目录（`state/session-<pid>`），退出即删除，
  换人使用时不需要额外操作，也不会有别人的 cookie 留在磁盘上。
- 需要免登录才显式加 `--keep-login`；此时登录态存在 `profile/`。
- `--switch` 会把 `profile/` 整个删掉，相当于退出登录。

## 为什么它现在能刷动

踩过的几个坑，都写在代码注释里：

1. **`snapshot()` 抛异常被当成"没有视频"**：`mediaTitle` 里 `text(querySelector(...))`
   在播放器页拿到 `null` 直接抛 `TypeError`，整次检测失败，于是无限"进去→弹回"。
   现在 `text()` 对空元素返回空串，`snapshot()` 也整体兜底。
2. **播放器在 Shadow DOM 里**：`<video>` 挂在自定义元素的 shadowRoot 下，普通
   `querySelectorAll` 穿不过去。`assets/player.js` 会把主文档、同源 iframe、
   所有 shadowRoot 一起收集再找媒体。
3. **站点要求先点播放**：直接调 `play()` 会被手势策略拒绝。`start()` 先点播放器区域，
   再驱动 media。
4. **倍速/静音会被站点改回去**：`enforce()` 每 500ms 对**所有**媒体元素重新落
   `playbackRate` 和 `muted`，页面自己的状态机改不掉。
5. **前两级目录白等**：轮询节拍改成自适应——没播放器时 2 秒一轮，播放中 5 秒。
6. **完成判定只认站点**：只有页面显示「已完成」才算学完。视频播到结尾、进度到 99%
   都不算——那时服务端还没记学时，提前切走等于白看。
7. **本地状态不能压过站点**：旧版本曾用 99% 误判记下一批 `done`，导致新版本一进
   课程就"本级已走完"来回空转。现在 `advance()` 只信站点给的完成标记，
   本地记录仅用于限制同一次运行里的重试次数。
8. **课程行显示 100% 却不跳过**：完成进度写在整行文字里，按钮自己只有"去学习"三个字。
   现在按整行文字解析百分比，≥99.5% 直接跳过。

## 反检测

`assets/anti-detect.js` 伪装可见性/焦点状态、拦截 `visibilitychange`/`blur`、
冻结"鼠标空闲"计时并持续产生移动，静音走元素层强制。启动参数另外关掉了
后台节流、遮挡检测（`CalculateNativeWinOcclusion`）、自动播放限制。

**注意**：不要把这些覆盖加到 `innerWidth/innerHeight` 上——页面永远以为自己还是
打开时那么大，窗口缩放和 `SetZoomFactor` 就全部失效了（这个坑已经踩过）。

## 子命令

```powershell
.\yuketang-autolearn.exe routes            # 看适配器路由表
.\yuketang-autolearn.exe status            # 看断点续刷状态
.\yuketang-autolearn.exe watch --url ...   # 盯一页：媒体数/iframe/播放状态，排障用
.\yuketang-autolearn.exe probe --url ...   # 落页面结构快照，改适配器用
```

## 目录结构

- `src/web.rs` — WebView2 宿主（STA 线程 + 消息泵、弹窗就地导航、播放器/日志分栏）
- `src/logging.rs` — 把 stdout 接到日志面板（匿名管道 + 行切分）
- `src/player.rs` — 刷课主循环（逐级进入、开播、完成判定、回退、跳过播不了的课）
- `assets/player.js` — 页面内控制层（找播放器、开播、倍速、静音、入口列表）
- `assets/anti-detect.js` — 反检测
- `adapters/yuketang.toml` — 路由与选择器（平台改版时改这里，不重编译）
- `make-icon.ps1` — 重新生成 `assets/app.ico`

## 许可

MIT，见 [LICENSE](LICENSE)。

## 构建

```powershell
cargo build --release
# 产物：target\release\yuketang-autolearn.exe
```

图标和版本信息由 `build.rs` + `winresource` 在构建时写进资源段（需要 rc.exe，
在 Windows SDK 的 bin 目录下；没找到只会警告，不影响运行）。
改图标就替换 `assets/app.ico`（用 `make-icon.ps1` 可以重新生成）。

## 排障

- 想留一份日志文件：设环境变量 `YKT_LOG_FILE=路径`，日志会同时写进该文件。
- 只想看页面状态：`yuketang-autolearn.exe watch --url <地址>`，
  会按秒打印媒体元素、iframe、播放状态。
- 调试版（`cargo build`）保留控制台；发布版是 GUI 子系统。
