// 注入到每个文档的播放器控制层。Rust 侧只调这里的函数，不直接碰页面 DOM。
(() => {
  if (window.__YKT) return;

  // 播放器常挂在同源 iframe 里（雨课堂的 fromProIframe），只扫主文档会永远找不到 media。
  // 新版学习空间还把播放器封在自定义元素里，<video> 落在 shadowRoot 底下，
  // 普通 querySelectorAll 穿不过去，所以这里把文档、同源 iframe、shadowRoot 一起收齐。
  const roots = () => {
    const out = [document];
    const seen = new Set();
    const pushRoot = (r) => {
      if (r && !seen.has(r)) {
        seen.add(r);
        out.push(r);
      }
    };
    const walkDoc = (d) => {
      for (const f of Array.from(d.querySelectorAll("iframe"))) {
        try {
          const fd = f.contentDocument;
          if (fd && !seen.has(fd)) {
            pushRoot(fd);
            walkDoc(fd);
          }
        } catch (e) {}
      }
    };
    const walkShadow = (root, depth) => {
      if (depth > 8) return;
      let els = [];
      try {
        els = Array.from(root.querySelectorAll("*"));
      } catch (e) {
        return;
      }
      for (const el of els) {
        if (el.shadowRoot) {
          pushRoot(el.shadowRoot);
          walkShadow(el.shadowRoot, depth + 1);
        }
      }
    };
    walkDoc(document);
    walkShadow(document, 0);
    for (let i = 0; i < out.length; i++) walkShadow(out[i], 0);
    return out;
  };

  const mediaIn = (root) => {
    let list = [];
    try {
      list = Array.from(root.querySelectorAll("video,audio"));
    } catch (e) {}
    return list;
  };

  const pick = () => {
    const list = roots().flatMap(mediaIn);
    if (!list.length) return null;
    // 优先挑"像主播放器"的：时长有效 > 尺寸最大 > 第一个
    return list.sort((a, b) => {
      const da = isFinite(a.duration) ? a.duration : -1;
      const db = isFinite(b.duration) ? b.duration : -1;
      if (da !== db) return db - da;
      const ra = a.getBoundingClientRect();
      const rb = b.getBoundingClientRect();
      return rb.width * rb.height - ra.width * ra.height;
    })[0];
  };

  const cssPath = (el) => {
    if (!el || !el.tagName) return "";
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1 && parts.length < 6) {
      let seg = node.tagName.toLowerCase();
      if (node.id) {
        parts.unshift(seg + "#" + node.id);
        break;
      }
      const cls = (typeof node.className === "string" ? node.className : "")
        .split(/\s+/)
        .filter((c) => c && c.length < 30 && !/[{}()]/.test(c))
        .slice(0, 2);
      if (cls.length) seg += "." + cls.join(".");
      const parent = node.parentElement;
      if (parent) {
        const same = Array.from(parent.children).filter((c) => c.tagName === node.tagName);
        if (same.length > 1) seg += ":nth-of-type(" + (same.indexOf(node) + 1) + ")";
      }
      parts.unshift(seg);
      node = node.parentElement;
    }
    return parts.join(" > ");
  };

  // 元素可能是 null（选择器没命中），这里必须兜住：以前 mediaTitle 里
  // text(querySelector(...)) 在视频页上抛 TypeError，整个 snapshot() 就挂了，
  // Rust 侧看到的是"没有媒体"，于是无限弹回，刷课永远不动。
  const text = (el, n) =>
    el
      ? ((el.innerText || el.textContent || "") + "").replace(/\s+/g, " ").trim().slice(0, n || 40)
      : "";

  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return false;
    const cs = getComputedStyle(el);
    return cs.visibility !== "hidden" && cs.display !== "none" && parseFloat(cs.opacity || "1") > 0.05;
  };

  // 控件栏在没悬停时通常是 opacity:0 但仍然真实存在，直接派发事件照样能触发
  // 站点自己的处理器。所以"能点"和"看得见"要分开判：只要求有尺寸、没 display:none。
  const clickable = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return false;
    const cs = getComputedStyle(el);
    return cs.visibility !== "hidden" && cs.display !== "none";
  };

  const fire = (el, type) => {
    el.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, view: window }));
  };

  const click = (el) => {
    el.scrollIntoView({ block: "center", inline: "center" });
    fire(el, "mousedown");
    fire(el, "mouseup");
    el.click();
  };

  // 侧栏那个「目录」会抢走 h1/h2，日志和状态表就全叫「目录」了。
  // 反过来，新版学习空间把 document.title 写死成「学习空间」，
  // 所以先看页面里的名字类元素，再退回标题；这些占位词一律跳过。
  const mediaTitle = (v) => {
    const d = v.ownerDocument || document;
    const generic = /^(目录|课程|学习|学习空间|学习中心|视频|播放|首页|雨课堂|详情)$/;
    const cand = [
      text(d.querySelector("[class*=video-name],[class*=lesson-name],[class*=chapter-name],[class*=course-name]"), 60),
      text(document.querySelector("[class*=video-name],[class*=lesson-name],[class*=chapter-name],[class*=course-name]"), 60),
      text(d.querySelector("h1,h2,[class*=title]"), 60),
      (d.title || "").trim(),
      (document.title || "").trim(),
    ];
    for (const c of cand) {
      const s = (c || "").trim();
      if (s && !generic.test(s)) return s.slice(0, 60);
    }
    // 都拿不到就退回页面第一段正文（视频页顶部就是课程名）
    const body = ((document.body && document.body.innerText) || "").replace(/\s+/g, " ").trim();
    return body ? body.slice(0, 40) : "当前项";
  };

  // 站点自己认的"已完成"标记才是准的，但页面上不止一处写"已完成"：
  // 左侧目录里已完成的其他课程也写着，之前就因此把还在播的视频误判成完成。
  // 所以只认"紧挨播放器上方那条标题栏"里的那一个。
  const siteDone = () => {
    const v = pick();
    if (!v) return { done: false, text: "" };
    const vr = v.getBoundingClientRect();
    for (const root of roots()) {
      let els = [];
      try {
        els = Array.from(root.querySelectorAll("span,div,p,em,i,strong"));
      } catch (e) {
        continue;
      }
      for (const el of els) {
        if (el.children && el.children.length) continue;
        const t = (el.textContent || "").replace(/\s+/g, "").trim();
        if (t !== "已完成" && t !== "已学完" && t !== "已完成学习") continue;
        if (!visible(el)) continue;
        // 位置判据：横向和视频有重叠，纵向贴着视频上沿（标题栏就在那儿），
        // 或者干脆落在视频区域内。
        const r = el.getBoundingClientRect();
        const overlapX = r.left < vr.right + 40 && r.right > vr.left - 40;
        const above = r.bottom <= vr.top + 10 && r.bottom > vr.top - 280;
        const inside = r.bottom > vr.top && r.top < vr.bottom;
        if (!(inside || (overlapX && above))) continue;
        // 再排一层：目录/章节树里的标签
        let n = el;
        let inList = false;
        for (let i = 0; i < 6 && n; i++) {
          const cls = (typeof n.className === "string" ? n.className : "") + "";
          if (/chapter|catalog|mulu|directory|tree|list|leaf|nav|route/i.test(cls)) {
            inList = true;
            break;
          }
          n = n.parentElement;
        }
        if (inList) continue;
        return { done: true, text: t };
      }
    }
    return { done: false, text: "" };
  };

  const snapshot = () => {
    try {
      const v = pick();
      const sd = siteDone();
      if (!v) return { found: false, mediaCount: 0, roots: roots().length, shadows: 0, siteDone: sd.done };
      const dur = isFinite(v.duration) ? v.duration : 0;
      return {
        found: true,
        mediaCount: roots().reduce((n, r) => n + mediaIn(r).length, 0),
        inShadow: !!(v.getRootNode && v.getRootNode() !== document && v.getRootNode().host),
        tag: v.tagName.toLowerCase(),
        currentTime: v.currentTime || 0,
        duration: dur,
        pct: dur > 0 ? ((v.currentTime || 0) / dur) * 100 : 0,
        paused: v.paused,
        ended: !!v.ended,
        siteDone: sd.done,
        doneText: sd.text,
        readyState: v.readyState,
        rate: v.playbackRate,
        muted: v.muted,
        src: (v.currentSrc || v.src || "").slice(0, 200),
        path: cssPath(v),
        title: mediaTitle(v),
      };
    } catch (e) {
      // 取标题之类的辅助信息失败时，也要把"确实找到播放器"这件事报出去，
      // 否则一次取值异常会被误判成"这一页没有视频"。
      return { found: true, degraded: true, err: String(e && e.message ? e.message : e) };
    }
  };

  // 开始播放。真机上拦路的东西有三样：站点要求"用户手势"、找不到原生控件、
  // 或者播放器外壳自己也要点一下才开始。所以这里按顺序全试一遍。
  const play = (rate) => {
    const v = pick();
    if (!v) {
      // 没有 media 时，先把播放器外壳点开（有的页面点了才挂 <video>）
      const poked = poke();
      return { ok: false, err: "no-media", poked };
    }
    const want = typeof rate === "number" && rate > 0 ? rate : 1;
    try {
      // 倍速：站点会在切集、弹提示时重置回 1，先落一次，后面定时器再兜
      window.__YKT_RATE = want;
      window.__YKT_WANT = want;
      const w = v.ownerDocument && v.ownerDocument.defaultView;
      if (w) w.__YKT_RATE = want;
      v.playbackRate = want;

      // 静音：有声播放没有用户手势会被浏览器拒绝，静音后一定放行
      v.muted = true;
      v.defaultMuted = true;
      v.volume = 0;

      const p = v.play();
      if (p && p.catch) {
        p.catch((e) => {
          window.__YKT_LAST_ERR = String(e && e.name ? e.name + ": " + e.message : e);
          // 被手势策略挡下时，补一次假鼠标事件再试
          poke(v);
          try {
            const p2 = v.play();
            if (p2 && p2.catch) p2.catch(() => {});
          } catch (_) {}
        });
      }
      return {
        ok: true,
        rate: v.playbackRate,
        muted: v.muted,
        paused: v.paused,
        currentTime: v.currentTime || 0,
        err: window.__YKT_LAST_ERR || "",
      };
    } catch (e) {
      return { ok: false, err: String(e) };
    }
  };

  // 点播放器外壳/控件，制造一次"用户手势"。优先点带播放语义的元素。
  const poke = (mediaEl) => {
    const scope = mediaEl && mediaEl.getRootNode ? mediaEl.getRootNode() : document;
    const bag = [];
    try {
      bag.push(...Array.from(scope.querySelectorAll("button,[role=button],[class*=play],[class*=btn],[class*=control]")));
    } catch (e) {}
    try {
      bag.push(...Array.from(document.querySelectorAll("[class*=play],[class*=video],[class*=player]")));
    } catch (e) {}
    for (const el of bag.slice(0, 12)) {
      const t = text(el, 12);
      if (/播放|play|开始/i.test(t) || /play/i.test((el.className || "") + "")) {
        if (window.__YKT_POKE) window.__YKT_POKE(el);
        try {
          el.click();
        } catch (e) {}
        return true;
      }
    }
    if (window.__YKT_POKE) window.__YKT_POKE(mediaEl || document.body);
    return false;
  };

  // 播放器外壳：<video> 往往是点了才开始挂，或者被 canvas 渲染接管。
  // 所以除了 media 元素，还要能认出"这块区域就是播放器"。
  const PLAYER_SEL = [
    "video",
    "canvas",
    "[class*=prism-player]",
    "[class*=prismPlayer]",
    "[class*=xgplayer]",
    "[class*=video-player]",
    "[class*=videoPlayer]",
    "[class*=player-container]",
    "[class*=player-wrap]",
    "[class*=vjs-]",
    "[class*=poster]",
    "[class*=play-btn]",
    "[class*=playBtn]",
    "[id*=player]",
    "[class*=ykt-video]",
  ].join(",");

  // 找出所有"像播放器"的最外层元素，按面积从大到小。
  const shells = () => {
    const found = [];
    const seen = new Set();
    for (const root of roots()) {
      let els = [];
      try {
        els = Array.from(root.querySelectorAll(PLAYER_SEL));
      } catch (e) {}
      for (const el of els) {
        // 只要最外层：祖先也命中的跳过，避免重复点
        let n = el.parentElement;
        let nested = false;
        while (n) {
          try {
            if (n.matches && n.matches(PLAYER_SEL)) {
              nested = true;
              break;
            }
          } catch (e) {}
          n = n.parentElement;
        }
        if (nested) continue;
        const r = el.getBoundingClientRect();
        if (r.width < 40 || r.height < 30) continue;
        const key = el;
        if (seen.has(key)) continue;
        seen.add(key);
        found.push({ el, area: r.width * r.height, w: r.width, h: r.height });
      }
    }
    return found.sort((a, b) => b.area - a.area);
  };

  // 在元素中心点一下。用 elementFromPoint 找真正盖在最上面的那个，
  // 否则会被透明遮罩吃掉点击（播放器常见）。
  const clickCenter = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) return false;
    const x = r.left + r.width / 2;
    const y = r.top + r.height / 2;
    let target = el;
    try {
      const top = (el.ownerDocument || document).elementFromPoint(x, y);
      if (top) target = top;
    } catch (e) {}
    const mk = (type, buttons) =>
      new MouseEvent(type, {
        bubbles: true,
        cancelable: true,
        view: window,
        clientX: x,
        clientY: y,
        screenX: x,
        screenY: y,
        button: 0,
        buttons,
      });
    for (const [type, buttons] of [
      ["pointerover", 0],
      ["pointerenter", 0],
      ["mousemove", 0],
      ["mousedown", 1],
      ["mouseup", 0],
      ["click", 0],
    ]) {
      try {
        target.dispatchEvent(mk(type, buttons));
      } catch (e) {}
    }
    try {
      target.click();
    } catch (e) {}
    return true;
  };

  // 站点要求"点一下才播"：直接调 play() 会被手势策略拒绝（NotAllowedError）。
  // 所以顺序是：找播放器外壳 → 点它中心 → media 没挂就等它挂 → 驱动播放。
  // 返回 started/via/err 供 Rust 侧判断要不要继续重试。
  const start = (rate) => {
    const want = typeof rate === "number" && rate > 0 ? rate : 1;
    window.__YKT_RATE = want;
    window.__YKT_WANT = want;

    // 1) 点播放器外壳（点播后 <video> 才创建，不点永远等不到）
    const sh = shells();
    let clicked = 0;
    for (const s of sh.slice(0, 3)) {
      if (clickCenter(s.el)) clicked++;
    }

    // 2) 驱动 media（点了之后手势已经算数）
    let v = pick();
    if (!v) {
      // 再补一次：有的播放器要大图标点中才挂 media
      for (const s of sh.slice(0, 2)) {
        const icon = s.el.querySelector("[class*=play],[class*=start]");
        if (icon) clickCenter(icon);
      }
      v = pick();
      if (!v) {
        return {
          started: false,
          via: "shell",
          clicked,
          shells: sh.length,
          err: "no-media-yet",
          paused: true,
          rate: 0,
        };
      }
    }

    const r = play(want);
    v = pick();
    const st = v
      ? {
          paused: v.paused,
          cur: Math.round((v.currentTime || 0) * 10) / 10,
          rate: v.playbackRate,
          muted: v.muted,
          rs: v.readyState,
        }
      : null;
    return {
      started: !!(st && st.paused === false),
      via: clicked ? "shell-click" : "media",
      clicked,
      shells: sh.length,
      rate: st ? st.rate : r.rate,
      paused: st ? st.paused : true,
      cur: st ? st.cur : 0,
      rs: st ? st.rs : -1,
      muted: st ? st.muted : true,
      err: r.err || "",
    };
  };

  // 站点靠 visibilitychange/blur 在最小化、切走时暂停播放。这里把"永远可见"伪装好，
  // 并在捕获阶段截住事件，页面自己的暂停监听器一个都跑不到。
  try {
    Object.defineProperty(document, "hidden", { get: () => false, configurable: true });
    Object.defineProperty(document, "visibilityState", { get: () => "visible", configurable: true });
    const swallow = (e) => e.stopImmediatePropagation();
    window.addEventListener("visibilitychange", swallow, true);
    document.addEventListener("visibilitychange", swallow, true);
    window.addEventListener("blur", swallow, true);
  } catch (e) {}

  const dismiss = (selectors) => {
    const hit = [];
    for (const sel of selectors || []) {
      let nodes = [];
      try {
        nodes = Array.from(document.querySelectorAll(sel));
      } catch (e) {
        continue;
      }
      for (const el of nodes) {
        if (!visible(el)) continue;
        const label = text(el, 20);
        if (/取消|关闭|放弃|cancel|close/i.test(label)) continue;
        hit.push(label || cssPath(el));
        click(el);
        break;
      }
    }
    return { dismissed: hit };
  };

  // 选择器除了 CSS 串，还支持 "text:去学习|开始学习" —— 按按钮文字找入口。
  // 雨课堂课程表里 24 行都叫"去学习"，所以标签取所在行的正文，序号才是身份。
  const rowLabel = (el) => {
    let n = el;
    for (let i = 0; i < 8 && n.parentElement; i++) {
      n = n.parentElement;
      const tag = n.tagName.toLowerCase();
      if (tag === "tr" || tag === "li" || /row|item|card|lesson|chapter|course/.test(n.className || "")) {
        const t = text(n, 50);
        if (t) return t;
      }
    }
    return text(el, 50);
  };

  const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

  const collect = (selectors) => {
    const out = [];
    const all = roots();
    const q = (d, sel) => {
      try {
        return Array.from(d.querySelectorAll(sel));
      } catch (e) {
        return [];
      }
    };
    const seenPath = new Set();
    for (const sel of selectors || []) {
      let nodes = [];
      if (sel.indexOf("text:") === 0) {
        const words = sel.slice(5).split("|").map((s) => s.trim()).filter(Boolean);
        if (!words.length) continue;
        const re = new RegExp("^(" + words.map(escapeRe).join("|") + ")$");
        nodes = all
          .flatMap((d) => q(d, "button,a,[role=button],[class*=button],[class*=btn]"))
          .filter((el) => visible(el) && re.test(text(el, 20)));
      } else {
        nodes = all.flatMap((d) => q(d, sel)).filter(visible);
      }
      for (const el of nodes) out.push({ el, label: rowLabel(el) });
    }
    // 同一个元素常被多条选择器命中（例如 .leaf-detail 与 .chapter-list .leaf-detail），
    // 不去重的话序号 i 与 DOM 顺序对不上，点"下一个"会点到已点过的。
    return out.filter((it) => {
      const p = cssPath(it.el);
      if (seenPath.has(p)) return false;
      seenPath.add(p);
      return true;
    });
  };

  // 视频页右侧目录里当前这条自带 active/current 类。点它等于把正在播的重载一遍，
  // 于是"没媒体 -> 点自己 -> 还没媒体"就死循环了，所以标出来让 Rust 侧跳过。
  const isActive = (el) => {
    let n = el;
    for (let i = 0; i < 4 && n; i++) {
      const cls = typeof n.className === "string" ? n.className : "";
      if (/active|current|playing|selected/i.test(cls)) return true;
      if (n.getAttribute && n.getAttribute("aria-current")) return true;
      n = n.parentElement;
    }
    return false;
  };

  // 按选择器列出候选学习项，带"是否已完成"的判断。
  // 关键：完成状态写在整行上（"必修 0 0 100% 去学习"），按钮自己只有"去学习"三个字，
  // 只看元素自身文字会把已完成的课当成没学，一遍遍点进去。
  const entries = (selectors) =>
    collect(selectors).map((item, i) => {
      const rowText = ((item.label || "") + " " + ((item.el.innerText || "") + ""))
        .replace(/\s+/g, " ")
        .trim();
      const m = rowText.match(/(\d{1,3}(?:\.\d+)?)\s*%/);
      const rowPct = m ? parseFloat(m[1]) : -1;
      const done =
        rowPct >= 99.5 ||
        /已学完|已完成|已观看|完成学习|complete|finished/i.test(
          rowText + " " + (typeof item.el.className === "string" ? item.el.className : "")
        );
      return {
        i,
        label: item.label,
        done,
        pct: rowPct,
        active: isActive(item.el),
        path: cssPath(item.el),
      };
    });

  const clickEntry = (selectors, index) => {
    const list = collect(selectors);
    const target = list[index];
    if (!target) return { ok: false, err: "entry-not-found", count: list.length };
    click(target.el);
    return { ok: true, label: target.label, count: list.length };
  };

  // 站点会在切集、弹提示时把倍速重置回 1。光靠定时器补不够快，
  // 直接劫持 playbackRate 的 setter：只要本窗口声明过倍速，谁写进来都落成那个值。
  try {
    const desc = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "playbackRate");
    if (desc && desc.set && !window.__YKT_RATE_HOOK) {
      window.__YKT_RATE_HOOK = 1;
      Object.defineProperty(HTMLMediaElement.prototype, "playbackRate", {
        configurable: true,
        get() {
          return desc.get.call(this);
        },
        set(v) {
          const w = this.ownerDocument && this.ownerDocument.defaultView;
          const want = (w && w.__YKT_RATE) || window.__YKT_RATE;
          desc.set.call(this, want && want > 0 ? want : v);
        },
      });
    }
  } catch (e) {}

  window.__YKT_RATE = window.__YKT_RATE || 1;

  // 按文字找可见的叶子元素（"1.00X" 这种倍速标签、菜单项都是纯文本节点）
  const pickText = (re, maxLen) => {
    for (const root of roots()) {
      let els = [];
      try {
        els = Array.from(root.querySelectorAll("button,a,li,span,div,p"));
      } catch (e) {
        continue;
      }
      for (const el of els) {
        // 控件栏常是 <div><span>1.00X</span><i/></div> 这种结构，
        // 只认"无子元素"会漏掉，所以允许 1~2 个子节点，靠文字长度兜住误命中。
        if (el.children && el.children.length > 2) continue;
        const t = (el.textContent || "").trim();
        if (!t || t.length > (maxLen || 8)) continue;
        if (!re.test(t)) continue;
        if (!clickable(el)) continue;
        return el;
      }
    }
    return null;
  };

  // 诊断用：把页面上"像倍速/音量的短文本"全列出来，方便定位控件文案。
  const speedLabels = () => {
    const out = [];
    for (const root of roots()) {
      let els = [];
      try {
        els = Array.from(root.querySelectorAll("button,a,li,span,div,p"));
      } catch (e) {
        continue;
      }
      for (const el of els) {
        if (el.children && el.children.length > 2) continue;
        const t = (el.textContent || "").trim();
        if (!t || t.length > 10) continue;
        if (!/[xX倍速率]/.test(t)) continue;
        out.push({
          t,
          cls: (typeof el.className === "string" ? el.className : "").slice(0, 60),
          vis: visible(el),
        });
      }
    }
    return out.slice(0, 30);
  };

  const SPEED_RE = /^\d+(\.\d+)?\s*[xX]$/;
  const speedText = (rate) => {
    const n = Number(rate);
    return [n.toFixed(2), n.toFixed(1), String(n)].map((x) => x + "X");
  };

  // 倍速：优先走站点自己的倍速菜单。原因很实际——站点把"当前倍速"存在自己的
  // 状态里，只改 <video>.playbackRate 的话，UI 仍显示 1.00X，而且它下一次同步
  // 会把元素改回 1；顺带心跳上报的 sp 字段也还是 1，容易被判异常。
  const setSpeed = (rate) => {
    const want = Number(rate);
    const wanted = speedText(want);
    const exact = (w) => new RegExp("^" + w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "$", "i");
    // 当前控件栏显示什么（严格可见的那个）
    const uiLabel = () => {
      const el = pickText(SPEED_RE, 8);
      const t = el ? (el.textContent || "").trim() : "";
      return { text: t, ok: wanted.some((w) => w.toLowerCase() === t.toLowerCase()) };
    };

    // 策略一：倍速菜单常驻在 DOM 里（未展开时只是透明度 0），
    // 直接点目标项就会走站点自己的处理器，这比改 playbackRate 更稳。
    for (const w of wanted) {
      const opt = pickText(exact(w), 8);
      if (!opt) continue;
      clickCenter(opt);
      const v = pick();
      if (v) {
        try {
          v.playbackRate = want;
        } catch (e) {}
      }
      const ui = uiLabel();
      if (ui.ok) return { ok: true, via: "option", text: w, ui: ui.text };
    }

    // 策略二：先悬停播放器唤出控件栏，再点一下当前倍速标签展开菜单，然后选目标项
    for (const s of shells().slice(0, 1)) {
      const r = s.el.getBoundingClientRect();
      try {
        const x = r.left + r.width / 2;
        const y = r.top + r.height * 0.92;
        const doc = s.el.ownerDocument || document;
        for (const type of ["pointermove", "mousemove", "mouseover"]) {
          doc.dispatchEvent(new MouseEvent(type, { bubbles: true, clientX: x, clientY: y, view: window }));
        }
      } catch (e) {}
    }
    const cur = pickText(SPEED_RE, 8);
    if (cur) clickCenter(cur);
    for (const w of wanted) {
      const opt = pickText(exact(w), 8);
      if (!opt) continue;
      clickCenter(opt);
      const ui = uiLabel();
      if (ui.ok) return { ok: true, via: "menu", text: w, ui: ui.text };
    }

    // 兜底：直接改元素倍速（UI 不跟随，但播放速率正确；enforce 会持续维持）
    const v = pick();
    if (v) {
      try {
        v.playbackRate = want;
      } catch (e) {}
    }
    return {
      ok: false,
      via: "element",
      err: "menu-not-found",
      ui: uiLabel().text,
      labels: speedLabels().map((x) => x.t).slice(0, 12),
    };
  };

  // 静音：点站点自己的音量按钮，让它的 UI 状态也变成静音，
  // 否则它会在下一次同步时把 muted 改回去（音量条回 1、声音出来）。
  const mute = () => {
    const sel =
      "[class*=volume],[class*=mute],[class*=sound],[class*=voice],button[aria-label*=音],button[title*=静音],button[title*=声音]";
    for (const root of roots()) {
      let els = [];
      try {
        els = Array.from(root.querySelectorAll(sel));
      } catch (e) {}
      for (const el of els) {
        if (!visible(el)) continue;
        const cls = (typeof el.className === "string" ? el.className : "") + "";
        if (/muted|is-mute|静音/i.test(cls) && !/unmute/i.test(cls)) {
          muteAll();
          return { ok: true, via: "already", cls };
        }
        clickCenter(el);
        muteAll();
        return { ok: true, via: "click", cls };
      }
    }
    muteAll();
    return { ok: false, via: "element", err: "no-button" };
  };

  const muteAll = () => {
    let n = 0;
    for (const root of roots()) {
      for (const v of mediaIn(root)) {
        try {
          v.muted = true;
          v.volume = 0;
          v.defaultMuted = true;
          n++;
        } catch (e) {}
      }
    }
    return n;
  };

  // 强制落实：站点会在切集、拖进度、弹提示、自己同步播放器状态时
  // 把 playbackRate 写回 1、把 muted 写回 false，所以不能只在开播那一刻设一次，
  // 必须对“所有”媒体元素反复落值——页面里常不止一个 video/audio。
  const enforce = () => {
    const want = window.__YKT_WANT || window.__YKT_RATE || 0;
    let hit = 0;
    for (const root of roots()) {
      for (const v of mediaIn(root)) {
        hit++;
        try {
          if (want > 0 && Math.abs(v.playbackRate - want) > 0.01) v.playbackRate = want;
        } catch (e) {}
        try {
          if (!v.muted) v.muted = true;
          if (v.volume > 0) v.volume = 0;
        } catch (e) {}
      }
    }
    // UI 状态也要跟上：站点自己的倍速标签/音量按钮如果显示 1.00X、未静音，
    // 说明站点状态机已经把值改回去了，这里重新按一次控件（限频，避免狂点）。
    if (want > 0) {
      const now = Date.now();
      if (now - (window.__YKT_UI_AT || 0) > 5000) {
        const cur = pickText(SPEED_RE, 8);
        const curText = cur ? (cur.textContent || "").trim() : "";
        const wanted = speedText(want);
        if (curText && !wanted.some((w) => w.toLowerCase() === curText.toLowerCase())) {
          window.__YKT_UI_AT = now;
          setSpeed(want);
        }
      }
    }
    return { elements: hit, want };
  };
  setInterval(enforce, 500);

  // 暴露给 Rust 侧做"落实确认"：返回每个媒体元素当前的真实倍速与静音状态，
  // 这样"倍速/静音没生效"能被日志直接看见，而不是靠猜。
  const audioState = () => {
    const out = [];
    for (const root of roots()) {
      for (const v of mediaIn(root)) {
        out.push({
          rate: v.playbackRate,
          muted: v.muted,
          volume: Math.round(v.volume * 100) / 100,
          paused: v.paused,
          cur: Math.round((v.currentTime || 0) * 10) / 10,
        });
      }
    }
    // 站点自己的倍速标签显示什么——"元素是 2 倍、UI 还是 1.00X"必须能一眼看出来
    const label = pickText(SPEED_RE, 8);
    return {
      want: window.__YKT_WANT || 0,
      uiSpeed: label ? (label.textContent || "").trim() : "",
      media: out,
    };
  };

  window.__YKT = {
    snapshot,
    play,
    start,
    poke,
    dismiss,
    entries,
    clickEntry,
    pick,
    roots,
    shells,
    enforce,
    audioState,
    setSpeed,
    mute,
    speedLabels,
  };
})();
