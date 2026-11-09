/* tasklane live map: repositories, tickets and agents as a graph, live or replayed.
   Canvas only, no libraries. All text from the server is written with textContent. */
(function () {
  var canvas = document.getElementById("map");
  if (!canvas) return;
  var ctx = canvas.getContext("2d");
  var wrap = canvas.parentElement;
  var tip = document.getElementById("maptip");
  var legend = document.getElementById("maplegend");
  var nowList = document.getElementById("mapnow-list");
  var bar = document.getElementById("mapbar");
  var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  var DPR = Math.min(window.devicePixelRatio || 1, 2), W = 0, H = 0;

  var AGENTS = [
    { key: "triage", label: "Triage agent", color: "#22d3ee", letter: "T" },
    { key: "develop", label: "Developer agent", color: "#fb923c", letter: "D" },
    { key: "review", label: "Reviewer agent", color: "#a78bfa", letter: "R" },
    { key: "ci", label: "CI", color: "#4ade80", letter: "C" },
    { key: "runner", label: "PR and merge", color: "#60a5fa", letter: "P" },
    { key: "you", label: "You (approvals)", color: "#fbbf24", letter: "Y" }
  ];
  var STATE_COLOR = { new: "#60a5fa", needs_info: "#fbbf24", duplicate_proposed: "#fbbf24", triaged: "#60a5fa", awaiting_approval: "#fbbf24",
    queued: "#60a5fa", developing: "#fb923c", rescoring: "#fb923c", pr_open: "#60a5fa", ci_running: "#4ade80", reviewing: "#a78bfa",
    merge_ready: "#4ade80", merged: "#34d399", failed: "#f87171", stopped: "#fbbf24" };
  var STATE_LABEL = { new: "New", needs_info: "Waiting for the reporter", duplicate_proposed: "Possible duplicate", triaged: "Planned", awaiting_approval: "Needs you",
    queued: "Queued", developing: "Developing", rescoring: "Checking the change", pr_open: "Pull request opened", ci_running: "CI running", reviewing: "In review",
    merge_ready: "Ready to merge", merged: "Merged", failed: "Failed", stopped: "Stopped" };
  function isClosed(s) { return s === "merged" || s.indexOf("closed_") === 0; }
  function agentFor(state) {
    switch (state) {
      case "new": case "triaged": return "triage";
      case "queued": case "developing": case "rescoring": return "develop";
      case "pr_open": case "merge_ready": return "runner";
      case "ci_running": return "ci";
      case "reviewing": return "review";
      case "awaiting_approval": case "failed": return "you";
      default: return null;
    }
  }
  function actionAgent(a) {
    switch (a) {
      case "ticket.triaged": case "ticket.resumed": case "ticket.text_changed": return "triage";
      case "ticket.developing": case "ticket.developed": return "develop";
      case "ticket.pr_opened": case "ticket.merged": return "runner";
      case "ticket.ci_green": case "ticket.ci_red": return "ci";
      case "ticket.reviewed": case "ticket.rescored_at_review": return "review";
      case "ticket.decided": return "you";
      default: return null;
    }
  }
  // the state a ticket is in after an event (used by the replay)
  function stateAfter(ev, prev) {
    var d = ev.d || {};
    switch (ev.action) {
      case "ticket.created": return "new";
      case "ticket.resumed": return "new";
      case "ticket.triaged": return d.to || "queued";
      case "ticket.decided": return d.decision === "approve" ? "queued" : "closed_rejected";
      case "ticket.developing": return "developing";
      case "ticket.developed": return d.to || "rescoring";
      case "ticket.text_changed": case "ticket.rescored_at_review": return "awaiting_approval";
      case "ticket.pr_opened": return "ci_running";
      case "ticket.ci_green": return "reviewing";
      case "ticket.ci_red": return (d.run || 1) >= 2 ? "failed" : "developing";
      case "ticket.reviewed": return d.verdict === "changes" ? "developing" : (d.verdict === "escalate" ? "awaiting_approval" : "merge_ready");
      case "ticket.merge_refused": return "ci_running";
      case "ticket.merged": return "merged";
      case "ticket.failed": return "failed";
      case "ticket.retried_by_person": return "queued";
      case "ticket.closed_by_person": return "closed_rejected";
      default: return prev;
    }
  }

  var repos = {}, tickets = {}, agents = {}, pulses = [], busyNow = {}, paused = false;
  var core = { x: 0, y: 0 };
  var mode = "live", replay = null;

  function hash(n) { var x = Math.sin(n * 127.1 + 311.7) * 43758.5453; return x - Math.floor(x); }
  function resize() {
    W = wrap.clientWidth; H = Math.max(520, Math.min(760, Math.round(W * 0.58)));
    canvas.style.width = W + "px"; canvas.style.height = H + "px";
    canvas.width = Math.round(W * DPR); canvas.height = Math.round(H * DPR);
    layout();
  }
  function layout() {
    // leave room on the right for the "Working now" panel on wide screens
    var Wu = W > 900 ? W - 330 : W;
    core.x = Wu / 2; core.y = H / 2;
    var m = Math.min(Wu, H);
    var list = Object.keys(repos).map(function (k) { return repos[k]; });
    list.forEach(function (r, i) {
      var a = -Math.PI / 2 + (i / Math.max(1, list.length)) * Math.PI * 2;
      var rad = m * (list.length === 1 ? 0 : 0.2);
      r.x = core.x + Math.cos(a) * rad; r.y = core.y + Math.sin(a) * rad;
    });
    AGENTS.forEach(function (a, i) {
      var ang = Math.PI + (i / AGENTS.length) * Math.PI * 2 + 0.2;
      var o = agents[a.key] || (agents[a.key] = { def: a, busy: 0 });
      o.x = core.x + Math.cos(ang) * (Wu * 0.4); o.y = core.y + Math.sin(ang) * (H * 0.37);
    });
  }

  function setRepos(list) {
    var seen = {};
    list.forEach(function (r) {
      var o = repos[r.id] || (repos[r.id] = { id: r.id, x: core.x, y: core.y });
      o.name = r.name; o.mode = r.mode; o.open = r.open; seen[r.id] = 1;
    });
    Object.keys(repos).forEach(function (k) { if (!seen[k]) delete repos[k]; });
    layout();
  }
  function ensureTicket(id, meta) {
    var t = tickets[id];
    if (!t) {
      var r = repos[meta.repo] || { x: core.x, y: core.y };
      t = tickets[id] = { id: id, x: r.x, y: r.y, tx: r.x, ty: r.y, born: performance.now(), alpha: 0 };
    }
    t.repo = meta.repo; t.title = meta.title; t.level = meta.level;
    return t;
  }
  function pulse(t, agentKey) {
    if (!agentKey || !agents[agentKey] || reduce) return;
    pulses.push({ id: t.id, agent: agentKey, t0: performance.now(), dur: mode === "replay" ? 700 : 1100, color: agents[agentKey].def.color });
    agents[agentKey].flash = performance.now();
  }

  // ---------------------------------------------------------------- live data
  var seenEvents = {};
  function pollLive() {
    if (mode !== "live" || document.hidden) return;
    fetch("/map/data", { credentials: "same-origin" }).then(function (r) {
      if (r.redirected || r.status === 401 || r.status === 303) { location.reload(); return null; }
      return r.ok ? r.json() : null;
    }).then(function (d) {
      if (!d || mode !== "live") return;
      paused = d.paused; setRepos(d.repos);
      var keep = {};
      d.tickets.forEach(function (x) {
        var t = ensureTicket(x.id, x); var changed = t.state && t.state !== x.state; t.state = x.state; t.age = x.age; keep[x.id] = 1;
        if (changed) pulse(t, agentFor(x.state));
      });
      Object.keys(tickets).forEach(function (k) { if (!keep[k]) delete tickets[k]; });
      busyNow = {}; d.busy.forEach(function (b) { (busyNow[b.agent] = busyNow[b.agent] || []).push(b); });
      var now = Math.floor(Date.now() / 1000);
      d.recent.forEach(function (e) {
        var key = e.ticket + "|" + e.action + "|" + (now - e.secs);
        if (!seenEvents[key]) { seenEvents[key] = 1; var t = tickets[e.ticket]; if (t) pulse(t, actionAgent(e.action)); }
      });
      updateNow();
    }).catch(function () {});
  }

  // ---------------------------------------------------------------- replay
  function loadReplay(hours) {
    fetch("/map/replay?hours=" + hours, { credentials: "same-origin" }).then(function (r) { return r.ok ? r.json() : null; }).then(function (d) {
      if (!d) return;
      var meta = {}; d.tickets.forEach(function (t) { meta[t.id] = t; });
      var from = d.now - hours * 3600; var t0 = d.events.length ? Math.max(from, d.events[0].t - 5) : from;
      replay = { events: d.events, meta: meta, t0: t0, t1: d.now, vt: t0, idx: 0, playing: false, speed: parseInt(document.getElementById("mp-speed").value, 10) || 60 };
      setRepos(d.repos); tickets = {}; pulses = []; busyNow = {};
      document.getElementById("mp-slider").value = 0; setPlaying(false); updateClock(); updateNow();
    });
  }
  function resetTo(vt, silent) {
    tickets = {}; pulses = []; replay.idx = 0; replay.vt = vt;
    while (replay.idx < replay.events.length && replay.events[replay.idx].t <= vt) { applyEvent(replay.events[replay.idx], silent); replay.idx++; }
  }
  function applyEvent(ev, silent) {
    var m = replay.meta[ev.ticket]; if (!m) return;
    var t = ensureTicket(ev.ticket, m);
    t.state = stateAfter(ev, t.state || "new");
    if (silent) { t.alpha = 1; var r = repos[m.repo]; if (r) { t.x = r.x; t.y = r.y; } return; }
    pulse(t, actionAgent(ev.action));
    if (actionAgent(ev.action)) { var a = actionAgent(ev.action); (agents[a].replayUntil = performance.now() + 2500); agents[a].replayTitle = m.title; }
  }
  function setPlaying(p) { if (!replay) return; replay.playing = p; var b = document.getElementById("mp-play"); b.querySelector("span").textContent = p ? "Pause" : "Play"; }
  function updateClock() {
    if (!replay) return;
    var d = new Date(replay.vt * 1000);
    document.getElementById("mp-clock").textContent = d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit" });
    document.getElementById("mp-slider").value = Math.round(((replay.vt - replay.t0) / Math.max(1, replay.t1 - replay.t0)) * 1000);
  }

  // ---------------------------------------------------------------- panel and legend
  function updateNow() {
    while (nowList.firstChild) nowList.removeChild(nowList.firstChild);
    var any = false;
    AGENTS.forEach(function (a) {
      var rows = [];
      if (mode === "live") (busyNow[a.key] || []).forEach(function (b) { rows.push(b.title); });
      else if (agents[a.key] && agents[a.key].replayUntil > performance.now()) rows.push(agents[a.key].replayTitle || "");
      rows.forEach(function (title) {
        any = true;
        var row = document.createElement("div"); row.className = "nowrow";
        var dot = document.createElement("i"); dot.style.background = a.color; row.appendChild(dot);
        var s1 = document.createElement("strong"); s1.textContent = a.label; row.appendChild(s1);
        var s2 = document.createElement("span"); s2.textContent = title; row.appendChild(s2);
        nowList.appendChild(row);
      });
    });
    if (!any) { var e = document.createElement("div"); e.textContent = paused && mode === "live" ? "Work is paused." : (mode === "live" ? "Nothing running." : "Nobody active at this moment."); nowList.appendChild(e); }
  }
  AGENTS.concat([{ label: "Needs you", color: "#fbbf24" }, { label: "Failed", color: "#f87171" }, { label: "Merged", color: "#34d399" }]).forEach(function (a) {
    var s = document.createElement("span"); s.className = "lg"; var i = document.createElement("i"); i.style.background = a.color; s.appendChild(i); s.appendChild(document.createTextNode(a.label)); legend.appendChild(s);
  });

  // ---------------------------------------------------------------- drawing
  function chip(text, x, y, color, strong) {
    ctx.font = (strong ? "600 " : "500 ") + "12px ui-monospace, Menlo, Consolas, monospace";
    var w = ctx.measureText(text).width + 16, h = 22, r = 11;
    ctx.fillStyle = "rgba(10,13,28,.82)"; ctx.strokeStyle = color; ctx.lineWidth = 1.2;
    ctx.beginPath(); ctx.moveTo(x - w / 2 + r, y - h / 2); ctx.arcTo(x + w / 2, y - h / 2, x + w / 2, y + h / 2, r); ctx.arcTo(x + w / 2, y + h / 2, x - w / 2, y + h / 2, r);
    ctx.arcTo(x - w / 2, y + h / 2, x - w / 2, y - h / 2, r); ctx.arcTo(x - w / 2, y - h / 2, x + w / 2, y - h / 2, r); ctx.closePath(); ctx.fill(); ctx.stroke();
    ctx.fillStyle = "#e8eaf6"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText(text, x, y + 1);
  }
  function glow(x, y, r, color, a) {
    var g = ctx.createRadialGradient(x, y, 0, x, y, r); g.addColorStop(0, color + Math.round(a * 255).toString(16).padStart(2, "0")); g.addColorStop(1, color + "00");
    ctx.fillStyle = g; ctx.beginPath(); ctx.arc(x, y, r, 0, 7); ctx.fill();
  }
  function ticketTarget(t, now) {
    var r = repos[t.repo]; if (!r) return;
    var ag = isClosed(t.state) ? null : agentFor(t.state);
    var ang = hash(t.id) * 6.283 + (reduce ? 0 : now / 9000 * (0.4 + hash(t.id + 3)));
    if (ag && agents[ag]) {
      var a = agents[ag], k = 0.62;
      t.tx = r.x + (a.x - r.x) * k + Math.cos(ang) * 16; t.ty = r.y + (a.y - r.y) * k + Math.sin(ang) * 16;
    } else {
      var rad = (isClosed(t.state) ? 70 : 38) + hash(t.id + 7) * 28;
      t.tx = r.x + Math.cos(ang) * rad; t.ty = r.y + Math.sin(ang) * rad;
    }
  }
  var last = performance.now();
  function frame(now) {
    var dt = Math.min(0.1, (now - last) / 1000); last = now;
    if (mode === "replay" && replay && replay.playing) {
      replay.vt += dt * replay.speed;
      // skip dead time: if nothing happens for a while, jump to just before the next event
      var nx = replay.events[replay.idx];
      if (nx && nx.t - replay.vt > 4 * replay.speed) replay.vt = nx.t - 0.5 * replay.speed;
      if (!nx && replay.t1 - replay.vt > 4 * replay.speed) replay.vt = replay.t1 - 0.5 * replay.speed;
      while (replay.idx < replay.events.length && replay.events[replay.idx].t <= replay.vt) { applyEvent(replay.events[replay.idx], false); replay.idx++; }
      if (replay.vt >= replay.t1) { replay.vt = replay.t1; setPlaying(false); }
      updateClock(); if ((now | 0) % 6 === 0) updateNow();
    }
    ctx.setTransform(DPR, 0, 0, DPR, 0, 0);
    var bg = ctx.createRadialGradient(W / 2, H / 2, 20, W / 2, H / 2, Math.max(W, H) * 0.7); bg.addColorStop(0, "#141a3a"); bg.addColorStop(1, "#080a18");
    ctx.fillStyle = bg; ctx.fillRect(0, 0, W, H);
    // stars
    ctx.fillStyle = "rgba(180,190,255,.25)";
    for (var s = 0; s < 70; s++) { ctx.fillRect(hash(s) * W, hash(s + 99) * H, 1.4, 1.4); }
    // links: core -> repos, repos -> agents they are feeding
    ctx.lineWidth = 1;
    Object.keys(repos).forEach(function (k) { var r = repos[k]; ctx.strokeStyle = "rgba(140,150,220,.22)"; ctx.beginPath(); ctx.moveTo(core.x, core.y); ctx.lineTo(r.x, r.y); ctx.stroke(); });
    var tl = Object.keys(tickets).map(function (k) { return tickets[k]; });
    tl.forEach(function (t) {
      ticketTarget(t, now); t.x += (t.tx - t.x) * Math.min(1, dt * 3); t.y += (t.ty - t.y) * Math.min(1, dt * 3);
      t.alpha = Math.min(1, t.alpha + dt * 2);
      var r = repos[t.repo]; var ag = isClosed(t.state) ? null : agentFor(t.state);
      if (r) { ctx.strokeStyle = "rgba(140,150,220,.10)"; ctx.beginPath(); ctx.moveTo(r.x, r.y); ctx.lineTo(t.x, t.y); ctx.stroke(); }
      if (ag && agents[ag]) { ctx.strokeStyle = agents[ag].def.color + "55"; ctx.lineWidth = 1.2; ctx.beginPath(); ctx.moveTo(t.x, t.y); ctx.lineTo(agents[ag].x, agents[ag].y); ctx.stroke(); ctx.lineWidth = 1; }
    });
    // pulses travel from the ticket to the agent
    pulses = pulses.filter(function (p) { return now - p.t0 < p.dur; });
    pulses.forEach(function (p) {
      var t = tickets[p.id], a = agents[p.agent]; if (!t || !a) return;
      var k = (now - p.t0) / p.dur, x = t.x + (a.x - t.x) * k, y = t.y + (a.y - t.y) * k;
      glow(x, y, 16, p.color, 0.9 * (1 - k * 0.4)); ctx.fillStyle = "#fff"; ctx.beginPath(); ctx.arc(x, y, 2.6, 0, 7); ctx.fill();
    });
    // repos
    Object.keys(repos).forEach(function (k) {
      var r = repos[k]; var col = r.mode === "full" ? "#34d399" : (r.mode === "triage" ? "#60a5fa" : "#64748b");
      glow(r.x, r.y, 38, col, 0.28); ctx.fillStyle = "#0e1230"; ctx.strokeStyle = col; ctx.lineWidth = 2; ctx.beginPath(); ctx.arc(r.x, r.y, 15, 0, 7); ctx.fill(); ctx.stroke();
      chip(r.name.split("/").pop(), r.x, r.y - 30, col, true);
    });
    // core
    glow(core.x, core.y, 70, "#fb923c", 0.35); ctx.fillStyle = "#fb923c"; ctx.beginPath(); ctx.arc(core.x, core.y, 17, 0, 7); ctx.fill();
    ctx.fillStyle = "#1b1206"; ctx.font = "700 14px system-ui, sans-serif"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText("t", core.x, core.y + 1);
    ctx.fillStyle = "rgba(232,234,246,.65)"; ctx.font = "500 11px ui-monospace, Menlo, monospace"; ctx.fillText(paused && mode === "live" ? "tasklane · paused" : "tasklane", core.x, core.y + 34);
    // agents
    AGENTS.forEach(function (def) {
      var a = agents[def.key]; if (!a) return;
      var live = mode === "live" ? (busyNow[def.key] || []).length > 0 : (a.replayUntil || 0) > now;
      var flash = a.flash ? Math.max(0, 1 - (now - a.flash) / 900) : 0;
      var pulseK = live ? 0.5 + 0.5 * Math.sin(now / 260) : 0;
      glow(a.x, a.y, 34 + pulseK * 14 + flash * 18, def.color, live ? 0.55 : 0.16 + flash * 0.4);
      ctx.fillStyle = "#0c1028"; ctx.strokeStyle = def.color; ctx.lineWidth = live ? 3 : 1.8; ctx.beginPath(); ctx.arc(a.x, a.y, 20, 0, 7); ctx.fill(); ctx.stroke();
      if (live) { ctx.strokeStyle = def.color + "88"; ctx.lineWidth = 1.5; ctx.beginPath(); ctx.arc(a.x, a.y, 26 + pulseK * 8, 0, 7); ctx.stroke(); }
      ctx.fillStyle = def.color; ctx.font = "700 15px system-ui, sans-serif"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText(def.letter, a.x, a.y + 1);
      chip(def.label, a.x, a.y + 38, def.color, live);
    });
    // tickets
    tl.forEach(function (t) {
      var col = STATE_COLOR[t.state] || "#64748b"; var closed = isClosed(t.state);
      ctx.globalAlpha = t.alpha * (closed ? 0.55 : 1);
      glow(t.x, t.y, closed ? 9 : 15, col, 0.5);
      ctx.fillStyle = col; ctx.beginPath(); ctx.arc(t.x, t.y, closed ? 3 : (t.level === 3 ? 6.5 : (t.level === 2 ? 5.5 : 4.5)), 0, 7); ctx.fill();
      if (t.state === "awaiting_approval" || t.state === "failed") { ctx.strokeStyle = col; ctx.lineWidth = 1.5; ctx.beginPath(); ctx.arc(t.x, t.y, 9 + (reduce ? 0 : 2 * Math.sin(now / 300)), 0, 7); ctx.stroke(); }
      ctx.globalAlpha = 1;
    });
    requestAnimationFrame(frame);
  }

  // ---------------------------------------------------------------- pointer
  function hit(mx, my) {
    var best = null, bd = 14;
    Object.keys(tickets).forEach(function (k) { var t = tickets[k], d = Math.hypot(t.x - mx, t.y - my); if (d < bd) { bd = d; best = { type: "ticket", o: t }; } });
    if (best) return best;
    AGENTS.forEach(function (def) { var a = agents[def.key]; if (a && Math.hypot(a.x - mx, a.y - my) < 24) best = { type: "agent", o: a }; });
    Object.keys(repos).forEach(function (k) { var r = repos[k]; if (Math.hypot(r.x - mx, r.y - my) < 18) best = { type: "repo", o: r }; });
    return best;
  }
  canvas.addEventListener("mousemove", function (e) {
    var b = canvas.getBoundingClientRect(), mx = e.clientX - b.left, my = e.clientY - b.top, h = hit(mx, my);
    canvas.style.cursor = h && h.type === "ticket" ? "pointer" : "default";
    if (!h) { tip.hidden = true; return; }
    while (tip.firstChild) tip.removeChild(tip.firstChild);
    var t1 = document.createElement("strong"), t2 = document.createElement("div");
    if (h.type === "ticket") { t1.textContent = h.o.title; t2.textContent = (STATE_LABEL[h.o.state] || h.o.state) + (h.o.level ? " · level " + h.o.level : ""); }
    else if (h.type === "agent") { t1.textContent = h.o.def.label; var bl = (busyNow[h.o.def.key] || []); t2.textContent = mode === "live" ? (bl.length ? "Working on: " + bl.map(function (x) { return x.title; }).join(", ") : "Idle") : "Replay"; }
    else { t1.textContent = h.o.name; t2.textContent = "Mode: " + h.o.mode + (h.o.open ? " · " + h.o.open + " open" : ""); }
    tip.appendChild(t1); tip.appendChild(t2); tip.hidden = false;
    tip.style.left = Math.min(W - 250, mx + 14) + "px"; tip.style.top = (my + 14) + "px";
  });
  canvas.addEventListener("mouseleave", function () { tip.hidden = true; });
  canvas.addEventListener("click", function (e) {
    var b = canvas.getBoundingClientRect(), h = hit(e.clientX - b.left, e.clientY - b.top);
    if (h && h.type === "ticket") location.href = "/tickets/" + h.o.id;
  });

  // ---------------------------------------------------------------- controls
  document.querySelectorAll("[data-map-mode]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      mode = btn.getAttribute("data-map-mode");
      document.querySelectorAll("[data-map-mode]").forEach(function (x) { x.setAttribute("aria-pressed", x === btn ? "true" : "false"); });
      bar.hidden = mode !== "replay"; tickets = {}; pulses = []; busyNow = {};
      if (mode === "replay") loadReplay(parseInt(document.getElementById("mp-range").value, 10)); else { replay = null; seenEvents = {}; pollLive(); }
      updateNow();
    });
  });
  document.getElementById("mp-play").addEventListener("click", function () {
    if (!replay) return; if (replay.vt >= replay.t1) resetTo(replay.t0, true); setPlaying(!replay.playing);
  });
  document.getElementById("mp-speed").addEventListener("change", function (e) { if (replay) replay.speed = parseInt(e.target.value, 10); });
  document.getElementById("mp-range").addEventListener("change", function (e) { loadReplay(parseInt(e.target.value, 10)); });
  document.getElementById("mp-slider").addEventListener("input", function (e) {
    if (!replay) return; var vt = replay.t0 + (replay.t1 - replay.t0) * (parseInt(e.target.value, 10) / 1000); resetTo(vt, true); updateClock(); updateNow();
  });
  window.addEventListener("resize", resize);
  resize(); pollLive(); setInterval(pollLive, 3000); requestAnimationFrame(frame);
  var rep = document.getElementById("mp-speed"); if (rep) rep.value = "60";
})();
