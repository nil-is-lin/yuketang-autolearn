(() => {
  // EME 钩子：requestMediaKeySystemAccess 一旦被调用就说明走了 DRM 通道。
  // 这段是幂等的，配合 add_init_script 可以在每个新文档里都装上。
  if (!window.__YKT_EME_HOOK) {
    window.__YKT_EME_HOOK = true;
    window.__YKT_EME = window.__YKT_EME || [];
    const orig = navigator.requestMediaKeySystemAccess && navigator.requestMediaKeySystemAccess.bind(navigator);
    if (orig) {
      navigator.requestMediaKeySystemAccess = function (keySystem, configs) {
        window.__YKT_EME.push({ keySystem, configs: JSON.parse(JSON.stringify(configs || [])), at: Date.now() });
        return orig(keySystem, configs);
      };
    }
  }

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
        const same = Array.from(parent.children).filter(
          (c) => c.tagName === node.tagName
        );
        if (same.length > 1) seg += ":nth-of-type(" + (same.indexOf(node) + 1) + ")";
      }
      parts.unshift(seg);
      node = node.parentElement;
    }
    return parts.join(" > ");
  };

  const text = (el, n) =>
    el
      ? ((el.innerText || el.textContent || "") + "").replace(/\s+/g, " ").trim().slice(0, n || 40)
      : "";

  // 同源 iframe 一起收；跨域的那份只能从 frames 里的 src 判断
  const docs = [document];
  for (const f of document.querySelectorAll("iframe")) {
    try {
      if (f.contentDocument) docs.push(f.contentDocument);
    } catch (e) {}
  }
  const qsa = (sel) =>
    docs.flatMap((d) => Array.from(d.querySelectorAll(sel)).map((el) => [el, d !== document]));

  const media = qsa("video,audio").map(([v, inFrame]) => ({
    tag: v.tagName.toLowerCase(),
    inFrame,
    hasMediaKeys: !!v.mediaKeys,
    id: v.id || "",
    cls: (typeof v.className === "string" ? v.className : "").slice(0, 120),
    src: (v.currentSrc || v.src || "").slice(0, 300),
    duration: v.duration === null ? null : (isFinite(v.duration) ? v.duration : -1),
    currentTime: v.currentTime,
    paused: v.paused,
    ended: v.ended,
    muted: v.muted,
    readyState: v.readyState,
    networkState: v.networkState,
    playbackRate: v.playbackRate,
    rect: (() => {
      const r = v.getBoundingClientRect();
      return { w: Math.round(r.width), h: Math.round(r.height) };
    })(),
    path: cssPath(v),
    playerRoot: (() => {
      let n = v;
      for (let i = 0; i < 6 && n.parentElement; i++) n = n.parentElement;
      return { tag: n.tagName.toLowerCase(), cls: (typeof n.className === "string" ? n.className : "").slice(0, 160) };
    })(),
  }));

  const canPlay = {};
  const probe = document.createElement("video");
  for (const t of [
    'video/mp4;codecs="avc1.42E01E"',
    'video/mp4;codecs="hvc1"',
    'application/x-mpegURL',
    'application/vnd.apple.mpegurl',
    'video/webm;codecs="vp8,vorbis"',
  ]) {
    canPlay[t] = probe.canPlayType(t) || "no";
  }

  const frameSel = /chapter|catalog|section|menu|list|item|unit|lesson|course|dir/i;
  const clickables = qsa(
    "a,li,tr,td,button,[role=button],[class*=chapter],[class*=catalog],[class*=lesson],[class*=course],[class*=item]"
  )
    .filter(([el]) => {
      const r = el.getBoundingClientRect();
      if (r.width < 4 || r.height < 4) return false;
      const label = text(el, 60);
      if (!label) return false;
      return frameSel.test((el.className || "") + " " + (el.id || "") + " " + el.tagName.toLowerCase());
    })
    .slice(0, 120)
    .map(([el, inFrame]) => ({
      tag: el.tagName.toLowerCase(),
      inFrame,
      label: text(el, 60),
      href: (el.getAttribute("href") || "").slice(0, 200),
      cls: (typeof el.className === "string" ? el.className : "").slice(0, 120),
      done: /complete|finished|done|yiwancheng|已学完|✓/i.test(
        (el.className || "") + (el.innerText || "")
      ),
      path: cssPath(el),
    }));

  // 按钮单独收一份：像"去学习"这种 class 里不含 course/chapter 的操作按钮，
  // 上一版被 frameSel 过滤掉了，导致自动刷课找不到入口。
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
    return "";
  };

  const seenPath = new Set();
  const buttons = qsa("button,a,[role=button],[class*=button],[class*=btn]")
    .filter(([el]) => {
      const r = el.getBoundingClientRect();
      if (r.width < 8 || r.height < 8 || el.disabled) return false;
      if (!text(el, 12)) return false;
      const p = cssPath(el);
      if (seenPath.has(p)) return false;
      seenPath.add(p);
      return true;
    })
    .slice(0, 200)
    .map(([el, inFrame]) => ({
      tag: el.tagName.toLowerCase(),
      txt: text(el, 12),
      row: rowLabel(el),
      cls: (typeof el.className === "string" ? el.className : "").slice(0, 80),
      inFrame,
      path: cssPath(el),
    }));

  const overlays = Array.from(document.querySelectorAll("div,section"))
    .filter((el) => {
      const cs = getComputedStyle(el);
      if (cs.position !== "fixed" && cs.position !== "absolute") return false;
      const r = el.getBoundingClientRect();
      if (r.width < 120 || r.height < 60) return false;
      if (parseFloat(cs.opacity || "1") < 0.05) return false;
      if (cs.visibility === "hidden" || cs.display === "none") return false;
      const z = parseInt(cs.zIndex || "0", 10);
      return z >= 10 || el.querySelector("button,[class*=close],[class*=confirm],[class*=btn]");
    })
    .slice(0, 12)
    .map((el) => ({
      cls: (typeof el.className === "string" ? el.className : "").slice(0, 160),
      text: text(el, 160),
      buttons: Array.from(el.querySelectorAll("button,[class*=btn],[class*=close]"))
        .map((b) => text(b, 20))
        .filter(Boolean)
        .slice(0, 8),
      path: cssPath(el),
    }));

  const resources = (performance.getEntriesByType("resource") || [])
    .filter((e) => ["xmlhttprequest", "fetch"].includes(e.initiatorType))
    .map((e) => e.name)
    .filter((u) => !/\.(js|css|png|jpg|jpeg|gif|svg|woff2?|ttf|ico|mp4|m3u8|ts)(\?|$)/i.test(u))
    .slice(-80);

  return JSON.stringify({
    url: location.href,
    title: document.title,
    now: new Date().toISOString(),
    viewport: { w: innerWidth, h: innerHeight, dpr: devicePixelRatio },
    docBytes: document.documentElement.outerHTML.length,
    media,
    canPlay,
    frames: Array.from(document.querySelectorAll("iframe")).map((f) => ({
      src: (f.src || "").slice(0, 300),
      id: f.id || "",
      cls: (typeof f.className === "string" ? f.className : "").slice(0, 120),
      path: cssPath(f),
    })),
    clickables,
    buttons,
    overlays,
    eme: window.__YKT_EME || [],
    sameOriginDocs: docs.length,
    resources,
    storage: {
      cookieNames: document.cookie.split(";").map((c) => c.split("=")[0].trim()).slice(0, 40),
      localKeys: (() => {
        try {
          return Object.keys(localStorage).slice(0, 60);
        } catch (e) {
          return [];
        }
      })(),
    },
  });
})()
