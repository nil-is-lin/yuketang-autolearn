// 反检测层：站点会通过"窗口是否可见/被遮挡、鼠标有没有在动、有没有人交互"
// 来判断页面是不是被脚本托管，一旦判定为后台或遮挡就暂停播放、丢弃心跳。
// 这里把判据统一伪装成"前台、可见、有人在操作"，其余交给播放器控制层。
//
// 这段脚本幂等：每个新文档都会跑一次（add_init_script），重复执行不会叠加副作用。
(() => {
  if (window.__YKT_GUARD) return;
  window.__YKT_GUARD = 1;

  const fixed = (obj, key, val) => {
    try {
      Object.defineProperty(obj, key, { get: () => val, configurable: true });
    } catch (e) {}
  };

  // 1) 可见性：最小化、切标签、被别的窗口盖住时，浏览器会把这两个值改掉。
  fixed(document, "hidden", false);
  fixed(document, "visibilityState", "visible");
  fixed(document, "webkitHidden", false);
  fixed(document, "webkitVisibilityState", "visible");
  fixed(document, "hasFocus", () => true);

  // 2) 焦点：一律回答"有焦点"，接管 blur 判定。
  //    注意不要改 getBoundingClientRect/getClientRects——那会让每个元素都被判成
  //    满屏可见，点击坐标和可见性判断全乱，反而点不到播放按钮。

  // 3) 鼠标空闲判定：站点常记录"多久没动鼠标"。这里让鼠标永远在缓慢移动，
  //    并把 lastActivity 之类的常见字段一直刷新成"刚刚"。
  const now = () => Date.now();
  window.__YKT_MOUSE = { x: 0, y: 0, at: now(), moving: true };
  const tick = () => {
    const w = window.innerWidth || 1360;
    const h = window.innerHeight || 900;
    const m = window.__YKT_MOUSE;
    m.x = Math.round(w / 2 + Math.sin(now() / 3000) * (w / 6));
    m.y = Math.round(h / 2 + Math.cos(now() / 4700) * (h / 6));
    m.at = now();
  };
  tick();
  setInterval(tick, 1000);

  // 把"最后活动时间"这类字段挂到 document/window 上，读到就是刚活动过。
  for (const key of ["lastActivity", "lastActiveTime", "lastMoveTime", "lastInteraction"]) {
    fixed(document, key, undefined);
    fixed(window, key, undefined);
  }

  // 4) 窗口尺寸不再伪装。
  //    之前把 innerWidth/innerHeight 锁成固定值，副作用很实在：
  //    页面永远以为自己还是打开时那么大，窗口缩放、控制器 SetZoomFactor 全都没反应，
  //    排版也不会重排。站点判"是否可见"靠的是 hidden/visibilityState，跟尺寸无关，
  //    所以这段留空更安全。

  // 5) 文档被遮挡的判定：IntersectionObserver / 可见性 API 一律回"完全可见"。
  try {
    const origIO = window.IntersectionObserver;
    if (origIO) {
      window.IntersectionObserver = class extends origIO {
        constructor(cb, opts) {
          super((entries, obs) => {
            cb(
              entries.map((e) => {
                try {
                  Object.defineProperty(e, "isIntersecting", { get: () => true, configurable: true });
                  Object.defineProperty(e, "intersectionRatio", { get: () => 1, configurable: true });
                } catch (_) {}
                return e;
              }),
              obs
            );
          }, opts);
        }
      };
    }
  } catch (e) {}

  try {
    if (document.visibilityState !== "visible") {
      // 某些页面在 load 阶段缓存了状态，这里补一次事件让依赖它的播放器恢复
      window.dispatchEvent(new Event("visibilitychange"));
    }
  } catch (e) {}

  // 6) 自动化痕迹：navigator.webdriver 一律为 false，插件列表给个非空值。
  try {
    Object.defineProperty(navigator, "webdriver", { get: () => false, configurable: true });
    Object.defineProperty(navigator, "plugins", {
      get: () => [{ name: "Chrome PDF Plugin" }, { name: "Chrome PDF Viewer" }],
      configurable: true,
    });
    Object.defineProperty(navigator, "languages", { get: () => ["zh-CN", "zh", "en"], configurable: true });
  } catch (e) {}

  // 7) 鼠标事件：播放器常要求"用户手势"才能开始播放、才能调倍速。
  //    这里补一套可信度较高的鼠标移动/按下/抬起事件，让手势判定通过。
  window.__YKT_POKE = (target) => {
    const el = target || document.body;
    if (!el) return false;
    const r = el.getBoundingClientRect();
    const x = r.left + Math.min(r.width / 2, 40);
    const y = r.top + Math.min(r.height / 2, 40);
    const mk = (type) =>
      new MouseEvent(type, {
        bubbles: true,
        cancelable: true,
        view: window,
        clientX: x,
        clientY: y,
        screenX: x,
        screenY: y,
        button: 0,
        buttons: type === "mouseup" ? 0 : 1,
      });
    for (const t of ["mouseover", "mousemove", "mousedown", "mouseup", "click"]) {
      try {
        el.dispatchEvent(mk(t));
      } catch (e) {}
    }
    return true;
  };

  // 8) 播放时的静音：站点检测到有声播放被拒绝会挂起，这里统一静音放行。
  //    muted 由控制层设置，这里只保证"静音不会被站点界面改回来"。
  try {
    const desc = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "muted");
    if (desc && desc.set && !window.__YKT_MUTE_HOOK) {
      window.__YKT_MUTE_HOOK = 1;
      Object.defineProperty(HTMLMediaElement.prototype, "muted", {
        configurable: true,
        get() {
          return desc.get.call(this);
        },
        set(_v) {
          // 一律静音：用户要的是刷课，不是听课
          desc.set.call(this, true);
        },
      });
    }
  } catch (e) {}
})();
