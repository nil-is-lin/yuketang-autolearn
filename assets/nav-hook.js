// 弹窗就地化：站点点课程/章节时用 window.open 打开新标签页。
// WebView2 里把这个弹窗"接住再由宿主导航"会让新文档拿不到 opener 和 referrer，
// 页面直接判成"非法进入"，停在空白或弹"请先购买课程"。
//
// 这里改成：window.open 直接在同一个窗口里跳转，等于站点自己的 JS 完成导航，
// 上下文（opener/referrer/sessionStorage 引用）全部保留，和人工点进去一致。
(() => {
  if (window.__YKT_NAV_HOOK) return;
  window.__YKT_NAV_HOOK = 1;

  const go = (url) => {
    try {
      const u = String(url || "");
      if (!u) return false;
      if (/^(javascript|about|blob|data):/i.test(u)) return false;
      window.location.assign(u);
      return true;
    } catch (e) {
      return false;
    }
  };

  // 1) window.open：新窗口一律变成本窗口跳转
  try {
    const orig = window.open;
    window.open = function (url, name, features) {
      if (url && go(url)) return window;
      return orig ? orig.apply(this, arguments) : null;
    };
  } catch (e) {}

  // 2) <a target="_blank"> 与 form target="_blank"：同样就地走
  try {
    document.addEventListener(
      "click",
      (ev) => {
        const a = ev.target && ev.target.closest ? ev.target.closest("a[href]") : null;
        if (!a) return;
        const target = (a.getAttribute("target") || "").toLowerCase();
        if (target !== "_blank" && target !== "_new") return;
        const href = a.href;
        if (!href || /^(javascript|about|blob|data):/i.test(href)) return;
        ev.preventDefault();
        ev.stopImmediatePropagation();
        go(href);
      },
      true
    );
  } catch (e) {}
})();
