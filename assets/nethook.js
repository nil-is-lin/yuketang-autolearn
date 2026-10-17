// 网络抓包 + 页面内回放钩子。
// 抓：记录同源 fetch/XHR 的 url、方法、请求头、请求体和响应片段，用来拿到线上真实的心跳 payload。
// 放：回放请求走页面自己的 fetch，这样 cookie 由浏览器带，Rust 侧不需要导出登录态。
(() => {
  if (window.__YKT_NET) return;

  const MAX = 400;
  const SKIP = /\.(js|css|png|jpe?g|gif|svg|woff2?|ttf|ico|mp4|m3u8|ts|webp|webm)(\?|$)/i;
  const rawFetch = window.fetch;
  let last = { state: "idle" };

  const cap = (s, n) => (typeof s === "string" ? s.slice(0, n) : "");
  const originOf = (u) => {
    try {
      return new URL(u, location.href).origin;
    } catch (e) {
      return "";
    }
  };

  const headersOf = (h) => {
    const out = {};
    if (!h) return out;
    if (typeof h.forEach === "function") {
      h.forEach((v, k) => {
        out[k] = v;
      });
      return out;
    }
    if (Array.isArray(h)) {
      for (const pair of h) {
        if (pair && pair.length >= 2) out[String(pair[0])] = String(pair[1]);
      }
      return out;
    }
    for (const k of Object.keys(h)) out[k] = String(h[k]);
    return out;
  };

  const recs = [];
  const push = (r) => {
    if (!r.url || SKIP.test(r.url)) return null;
    if (recs.length >= MAX) recs.shift();
    recs.push(r);
    return r;
  };

  const origFetch = rawFetch;
  if (origFetch) {
    window.fetch = function (input, init) {
      const url =
        typeof input === "string" || input instanceof URL
          ? String(input)
          : (input && input.url) || String(location.href);
      const method = String(
        (init && init.method) || (input && input.method) || "GET"
      ).toUpperCase();
      const bodyRaw =
        init && init.body !== undefined
          ? init.body
          : typeof input === "object" && input && input.body !== undefined
            ? input.body
            : null;
      const rec = push({
        t: Date.now(),
        url,
        method,
        headers: headersOf((init && init.headers) || (input && input.headers)),
        body: cap(typeof bodyRaw === "string" ? bodyRaw : String(bodyRaw || ""), 12000),
        origin: originOf(url),
        kind: "fetch",
      });
      const p = origFetch.apply(this, arguments);
      if (rec) {
        p.then(
          (res) => {
            rec.status = res.status;
            try {
              res
                .clone()
                .text()
                .then((tx) => {
                  rec.resp = cap(tx, 4000);
                })
                .catch(() => {});
            } catch (e) {}
          },
          (e) => {
            rec.error = String(e);
          }
        );
      }
      return p;
    };
  }

  const XOpen = XMLHttpRequest.prototype.open;
  const XSend = XMLHttpRequest.prototype.send;
  const XSetH = XMLHttpRequest.prototype.setRequestHeader;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__yktNet = { method: String(method).toUpperCase(), url: String(url), headers: {} };
    return XOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.setRequestHeader = function (k, v) {
    if (this.__yktNet) this.__yktNet.headers[String(k)] = String(v);
    return XSetH.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function (body) {
    const x = this;
    const meta = x.__yktNet || { headers: {} };
    const rec = push({
      t: Date.now(),
      url: meta.url || x.responseURL,
      method: meta.method || "POST",
      headers: meta.headers,
      body: cap(typeof body === "string" ? body : "", 12000),
      origin: originOf(meta.url || x.responseURL),
      kind: "xhr",
    });
    if (rec) {
      x.addEventListener("loadend", () => {
        rec.status = x.status;
        try {
          rec.resp =
            x.responseType && x.responseType !== "text" ? "" : cap(x.responseText, 4000);
        } catch (e) {}
      });
    }
    return XSend.apply(this, arguments);
  };

  // fetch 里禁止脚本设置的头：浏览器会静默丢掉，留着只会误导 replay
  const FORBIDDEN = [
    "cookie",
    "content-length",
    "host",
    "origin",
    "referer",
    "connection",
    "keep-alive",
    "upgrade-insecure-requests",
    "date",
    "sec-",
    "access-control-",
  ];
  const allowed = (headers) => {
    const out = {};
    for (const [k, v] of Object.entries(headers || {})) {
      const lk = String(k).toLowerCase();
      if (FORBIDDEN.some((f) => lk === f || lk.startsWith(f))) continue;
      out[k] = v;
    }
    return out;
  };

  const send = (spec) => {
    if (!origFetch) return JSON.stringify({ state: "error", err: "no-fetch" });
    const method = String((spec && spec.method) || "POST").toUpperCase();
    const init = {
      method,
      credentials: "include",
      headers: allowed(spec.headers),
    };
    if (method !== "GET" && method !== "HEAD" && spec.body != null) init.body = spec.body;
    last = { state: "pending", url: spec.url, method, started: Date.now() };
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), spec.timeout_ms || 15000);
    init.signal = ctrl.signal;
    origFetch
      .call(window, spec.url, init)
      .then((res) =>
        res
          .text()
          .then((tx) => {
            clearTimeout(timer);
            last = {
              state: "done",
              url: spec.url,
              method,
              status: res.status,
              resp: tx.slice(0, 6000),
            };
          })
          .catch((e) => {
            clearTimeout(timer);
            last = { state: "done", url: spec.url, method, status: res.status, resp: "", err: String(e) };
          })
      )
      .catch((e) => {
        clearTimeout(timer);
        last = { state: "error", url: spec.url, method, err: String(e) };
      });
    return JSON.stringify({ started: true, url: spec.url, method });
  };

  // patterns 为空数组时返回全部，省得 Rust 侧再猜关键词
  const dump = (patterns) => {
    const list = Array.isArray(patterns) && patterns.length ? patterns : null;
    const hit = recs.filter((r) => !list || list.some((p) => r.url.indexOf(p) >= 0));
    return JSON.stringify({ href: location.href, total: recs.length, hits: hit });
  };

  window.__YKT_NET = { dump, send, last: () => JSON.stringify(last), reset: () => (recs.length = 0) };
})();
