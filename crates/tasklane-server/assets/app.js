/* tasklane console: small enhancements only. Every page works without this file. */
(function () {
  var root = document.documentElement;
  // theme: stored choice, otherwise the system setting (handled in CSS)
  // dark is the designed look; light only if the user chose it
  root.setAttribute("data-theme", "dark");
  try { var t = localStorage.getItem("tl-theme"); if (t) root.setAttribute("data-theme", t); } catch (e) {}
  document.addEventListener("click", function (e) {
    var b = e.target.closest("[data-theme-toggle]");
    if (b) {
      var dark = root.getAttribute("data-theme") === "dark";
      var next = dark ? "light" : "dark";
      root.setAttribute("data-theme", next);
      try { localStorage.setItem("tl-theme", next); } catch (e2) {}
    }
    var c = e.target.closest("[data-confirm]");
    if (c && !window.confirm(c.getAttribute("data-confirm"))) { e.preventDefault(); }
    var cp = e.target.closest("[data-copy]");
    if (cp) { navigator.clipboard && navigator.clipboard.writeText(cp.getAttribute("data-copy")); toast("Copied"); }
    var ex = e.target.closest("[data-expand-all]");
    if (ex) { document.querySelectorAll(ex.getAttribute("data-expand-all") + " details").forEach(function (d) { d.open = ex.dataset.state !== "open"; }); ex.dataset.state = ex.dataset.state === "open" ? "" : "open"; }
  });
  // double submit guard: a form posts once
  document.addEventListener("submit", function (e) {
    var f = e.target;
    if (f.dataset.busy) { e.preventDefault(); return; }
    f.dataset.busy = "1";
    f.querySelectorAll("button").forEach(function (b) { setTimeout(function () { b.disabled = true; }, 0); });
  });
  function toast(msg, bad) {
    var el = document.createElement("div"); el.className = "toast" + (bad ? " bad" : ""); el.textContent = msg;
    document.body.appendChild(el); setTimeout(function () { el.remove(); }, 2600);
  }
  // live regions: re-fetch an HTML fragment while the tab is visible
  document.querySelectorAll("[data-live]").forEach(function (el) {
    var url = el.getAttribute("data-live"), every = parseInt(el.getAttribute("data-every") || "5000", 10), busy = false;
    function tick() {
      if (document.hidden || busy) return;
      busy = true;
      fetch(url, { credentials: "same-origin", headers: { "X-Requested-With": "live" } }).then(function (r) {
        if (r.redirected || r.status === 401 || r.status === 303) { location.reload(); return null; }
        return r.ok ? r.text() : null;
      }).then(function (html) {
        if (html !== null && html !== el.__last) { el.innerHTML = html; el.__last = html; }
      }).catch(function () {}).then(function () { busy = false; });
    }
    setInterval(tick, every);
    document.addEventListener("visibilitychange", function () { if (!document.hidden) tick(); });
  });
  // relative filter on tables / lists
  document.querySelectorAll("[data-filter]").forEach(function (inp) {
    inp.addEventListener("input", function () {
      var q = inp.value.toLowerCase(), sel = inp.getAttribute("data-filter");
      document.querySelectorAll(sel).forEach(function (row) { row.style.display = row.textContent.toLowerCase().indexOf(q) >= 0 ? "" : "none"; });
    });
  });
  // a form that only hands the browser on to another site (the GitHub App creation) submits itself
  document.querySelectorAll("form[data-autosubmit]").forEach(function (f) { setTimeout(function () { f.submit(); }, 150); });
  // segmented controls submit their form on choice
  document.querySelectorAll("form[data-auto] select").forEach(function (s) { s.addEventListener("change", function () { s.form.submit(); }); });
})();
