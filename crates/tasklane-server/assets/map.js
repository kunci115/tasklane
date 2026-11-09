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
    { key: "triage", label: "Triage agent", color: "#00e5ff", letter: "T" },
    { key: "develop", label: "Developer agent", color: "#ff8a3d", letter: "D" },
    { key: "review", label: "Reviewer agent", color: "#b36bff", letter: "R" },
    { key: "ci", label: "CI", color: "#39ffb6", letter: "C" },
    { key: "runner", label: "PR and merge", color: "#4d7cff", letter: "P" },
    { key: "you", label: "You (approvals)", color: "#ffb800", letter: "Y" }
  ];
  var STATE_COLOR = { new: "#38bdf8", needs_info: "#ffb800", duplicate_proposed: "#ffb800", triaged: "#38bdf8", awaiting_approval: "#ffb800",
    queued: "#38bdf8", developing: "#ff8a3d", rescoring: "#ff8a3d", pr_open: "#38bdf8", ci_running: "#39ffb6", reviewing: "#b36bff",
    merge_ready: "#39ffb6", merged: "#2ee6a8", failed: "#ff3b6b", stopped: "#ffb800" };
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
        if (changed) { pulse(t, agentFor(x.state)); stateEffect(t, x.state); }
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
    var before = t.state;
    t.state = stateAfter(ev, t.state || "new");
    if (!silent && before !== t.state) stateEffect(t, t.state);
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

  // ---------------------------------------------------------------- effects
  var ripples = [], sparks = [], dust = [], hover = null;
  for (var di = 0; di < 70; di++) dust.push({ x: hash(di + 5), y: hash(di + 61), vx: (hash(di + 9) - .5) * .012, vy: (hash(di + 13) - .5) * .012, r: .6 + hash(di + 3) * 1.4 });
  function ripple(x, y, color, max, dur) { if (reduce) return; ripples.push({ x: x, y: y, color: color, max: max || 46, t0: performance.now(), dur: dur || 1100 }); }
  function burst(x, y, color) {
    if (reduce) return;
    for (var i = 0; i < 22; i++) { var a = Math.random() * 6.283, v = 30 + Math.random() * 90; sparks.push({ x: x, y: y, vx: Math.cos(a) * v, vy: Math.sin(a) * v, life: 0.7 + Math.random() * 0.6, age: 0, color: color }); }
    ripple(x, y, color, 70, 1300);
  }
  // effects that follow a ticket's change of state
  function stateEffect(t, state) {
    var c = STATE_COLOR[state] || "#38bdf8";
    if (state === "merged") burst(t.x, t.y, "#39ffb6");
    else if (state === "failed") { ripple(t.x, t.y, "#ff3b6b", 60, 1200); setTimeout(function () { ripple(t.x, t.y, "#ff3b6b", 60, 1200); }, 260); }
    else if (state === "awaiting_approval") ripple(t.x, t.y, "#ffb800", 56, 1300);
    else ripple(t.x, t.y, c, 38, 900);
  }

  // ---------------------------------------------------------------- drawing
  function chip(text, x, y, color, strong) {
    ctx.font = (strong ? "700 " : "500 ") + "12px 'JetBrains Mono', ui-monospace, Menlo, monospace";
    var w = ctx.measureText(text).width + 16, h = 22, r = 11;
    ctx.fillStyle = "rgba(4,7,14,.86)"; ctx.strokeStyle = color; ctx.lineWidth = 1.2;
    ctx.beginPath(); ctx.moveTo(x - w / 2 + r, y - h / 2); ctx.arcTo(x + w / 2, y - h / 2, x + w / 2, y + h / 2, r); ctx.arcTo(x + w / 2, y + h / 2, x - w / 2, y + h / 2, r);
    ctx.arcTo(x - w / 2, y + h / 2, x - w / 2, y - h / 2, r); ctx.arcTo(x - w / 2, y - h / 2, x + w / 2, y - h / 2, r); ctx.closePath(); ctx.fill(); ctx.stroke();
    ctx.fillStyle = "#e6f4ff"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText(text, x, y + 1);
  }
  function glow(x, y, r, color, a) {
    var g = ctx.createRadialGradient(x, y, 0, x, y, r); g.addColorStop(0, color + Math.round(Math.max(0, Math.min(1, a)) * 255).toString(16).padStart(2, "0")); g.addColorStop(1, color + "00");
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
    ctx.globalCompositeOperation = "source-over";
    var bg = ctx.createRadialGradient(W / 2, H / 2, 20, W / 2, H / 2, Math.max(W, H) * 0.72); bg.addColorStop(0, "#08121f"); bg.addColorStop(1, "#03050a");
    ctx.fillStyle = bg; ctx.fillRect(0, 0, W, H);
    // grid
    ctx.strokeStyle = "rgba(0,229,255,.045)"; ctx.lineWidth = 1;
    for (var gx = 0; gx < W; gx += 44) { ctx.beginPath(); ctx.moveTo(gx + .5, 0); ctx.lineTo(gx + .5, H); ctx.stroke(); }
    for (var gy = 0; gy < H; gy += 44) { ctx.beginPath(); ctx.moveTo(0, gy + .5); ctx.lineTo(W, gy + .5); ctx.stroke(); }
    // drifting dust
    ctx.fillStyle = "rgba(120,235,255,.30)";
    dust.forEach(function (d) { if (!reduce) { d.x += d.vx * dt; d.y += d.vy * dt; if (d.x < 0) d.x += 1; if (d.x > 1) d.x -= 1; if (d.y < 0) d.y += 1; if (d.y > 1) d.y -= 1; } ctx.fillRect(d.x * W, d.y * H, d.r, d.r); });
    // a scan line sweeping down the map
    if (!reduce) { var sy = ((now / 6500) % 1) * (H + 80) - 40, sg = ctx.createLinearGradient(0, sy - 40, 0, sy + 40); sg.addColorStop(0, "rgba(0,229,255,0)"); sg.addColorStop(.5, "rgba(0,229,255,.07)"); sg.addColorStop(1, "rgba(0,229,255,0)"); ctx.fillStyle = sg; ctx.fillRect(0, sy - 40, W, 80); }
    var tl = Object.keys(tickets).map(function (k) { return tickets[k]; });
    // move tickets first so every later layer uses this frame's positions
    tl.forEach(function (t) {
      ticketTarget(t, now); t.x += (t.tx - t.x) * Math.min(1, dt * 3); t.y += (t.ty - t.y) * Math.min(1, dt * 3);
      t.alpha = Math.min(1, t.alpha + dt * 2);
      if (t.fx === undefined) { t.fx = 1; if (t.alpha < 1 && t.state && !isClosed(t.state)) ripple(t.x, t.y, STATE_COLOR[t.state] || "#38bdf8", 40, 1000); }
    });
    // links: core -> repos
    ctx.lineWidth = 1;
    Object.keys(repos).forEach(function (k) { var r = repos[k]; ctx.strokeStyle = "rgba(0,229,255,.20)"; ctx.beginPath(); ctx.moveTo(core.x, core.y); ctx.lineTo(r.x, r.y); ctx.stroke(); });
    // faint constellation between nearby tickets, like a neural net
    if (tl.length <= 160) {
      ctx.lineWidth = 1;
      for (var i = 0; i < tl.length; i++) for (var j = i + 1; j < tl.length; j++) {
        var dx = tl[i].x - tl[j].x, dy = tl[i].y - tl[j].y, d2 = dx * dx + dy * dy;
        if (d2 < 3600 && tl[i].repo === tl[j].repo) { ctx.strokeStyle = "rgba(120,235,255," + (0.10 * (1 - d2 / 3600)).toFixed(3) + ")"; ctx.beginPath(); ctx.moveTo(tl[i].x, tl[i].y); ctx.lineTo(tl[j].x, tl[j].y); ctx.stroke(); }
      }
    }
    // repo -> ticket threads, and the flowing data line from each assigned ticket to its agent
    tl.forEach(function (t) {
      var r = repos[t.repo], ag = isClosed(t.state) ? null : agentFor(t.state);
      if (r) { ctx.strokeStyle = "rgba(0,229,255,.08)"; ctx.beginPath(); ctx.moveTo(r.x, r.y); ctx.lineTo(t.x, t.y); ctx.stroke(); }
      if (ag && agents[ag]) {
        var A = agents[ag], col = A.def.color, hot = hover && (hover.o === t || hover.o === A);
        ctx.strokeStyle = col + (hot ? "cc" : "55"); ctx.lineWidth = hot ? 1.8 : 1.2;
        ctx.setLineDash([5, 7]); ctx.lineDashOffset = reduce ? 0 : -now / 40;
        ctx.beginPath(); ctx.moveTo(t.x, t.y); ctx.lineTo(A.x, A.y); ctx.stroke(); ctx.setLineDash([]); ctx.lineWidth = 1;
        if (!reduce) { // a bead of light travelling along the line
          var k = ((now / 1700) + hash(t.id)) % 1, px = t.x + (A.x - t.x) * k, py = t.y + (A.y - t.y) * k;
          ctx.globalCompositeOperation = "lighter"; glow(px, py, 9, col, .85); ctx.globalCompositeOperation = "source-over";
        }
      }
    });
    // pulses leave a comet tail
    var ended = [];
    pulses = pulses.filter(function (p) { var live = now - p.t0 < p.dur; if (!live) ended.push(p); return live; });
    ended.forEach(function (p) { var a = agents[p.agent]; if (a) ripple(a.x, a.y, p.color, 52, 900); });
    ctx.globalCompositeOperation = "lighter";
    pulses.forEach(function (p) {
      var t = tickets[p.id], a = agents[p.agent]; if (!t || !a) return;
      var k = (now - p.t0) / p.dur, x = t.x + (a.x - t.x) * k, y = t.y + (a.y - t.y) * k;
      p.trail = p.trail || []; p.trail.push([x, y]); if (p.trail.length > 16) p.trail.shift();
      for (var q = 0; q < p.trail.length; q++) { var f = q / p.trail.length; glow(p.trail[q][0], p.trail[q][1], 4 + 9 * f, p.color, .55 * f); }
      glow(x, y, 20, p.color, .95); ctx.fillStyle = "#fff"; ctx.beginPath(); ctx.arc(x, y, 2.8, 0, 7); ctx.fill();
    });
    ctx.globalCompositeOperation = "source-over";
    // heartbeat rings from the core
    if (!reduce) for (var h = 0; h < 2; h++) { var hk = ((now / 3200) + h * .5) % 1; ctx.strokeStyle = "rgba(255,46,147," + (0.32 * (1 - hk)).toFixed(3) + ")"; ctx.lineWidth = 1.5; ctx.beginPath(); ctx.arc(core.x, core.y, 18 + hk * 70, 0, 7); ctx.stroke(); }
    // repos: a ring that fills with how much is open
    Object.keys(repos).forEach(function (k) {
      var r = repos[k]; var col = r.mode === "full" ? "#39ffb6" : (r.mode === "triage" ? "#00e5ff" : "#56677f");
      var hot = hover && hover.o === r;
      ctx.globalCompositeOperation = "lighter"; glow(r.x, r.y, hot ? 54 : 40, col, hot ? .4 : .26); ctx.globalCompositeOperation = "source-over";
      ctx.fillStyle = "#050812"; ctx.strokeStyle = col; ctx.lineWidth = 2; ctx.beginPath(); ctx.arc(r.x, r.y, 15, 0, 7); ctx.fill(); ctx.stroke();
      var frac = Math.min(1, (r.open || 0) / 10);
      if (frac > 0) { ctx.strokeStyle = col; ctx.lineWidth = 3; ctx.lineCap = "round"; ctx.beginPath(); ctx.arc(r.x, r.y, 22, -Math.PI / 2, -Math.PI / 2 + frac * 6.283); ctx.stroke(); ctx.lineCap = "butt"; }
      chip(r.name.split("/").pop(), r.x, r.y - 33, col, true);
    });
    // core
    ctx.globalCompositeOperation = "lighter"; glow(core.x, core.y, 80, "#ff2e93", .42); ctx.globalCompositeOperation = "source-over";
    ctx.fillStyle = "#ff2e93"; ctx.beginPath(); ctx.arc(core.x, core.y, 17, 0, 7); ctx.fill();
    ctx.strokeStyle = "rgba(255,255,255,.35)"; ctx.lineWidth = 1; ctx.beginPath(); ctx.arc(core.x, core.y, 21, 0, 7); ctx.stroke();
    ctx.fillStyle = "#1a0410"; ctx.font = "700 14px 'JetBrains Mono', monospace"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText("t", core.x, core.y + 1);
    ctx.fillStyle = "rgba(230,244,255,.7)"; ctx.font = "500 11px 'JetBrains Mono', ui-monospace, monospace"; ctx.fillText(paused && mode === "live" ? "tasklane · paused" : "tasklane", core.x, core.y + 36);
    // agents: busy ones glow, spin a dashed orbit and sweep like a radar
    AGENTS.forEach(function (def) {
      var a = agents[def.key]; if (!a) return;
      var live = mode === "live" ? (busyNow[def.key] || []).length > 0 : (a.replayUntil || 0) > now;
      var flash = a.flash ? Math.max(0, 1 - (now - a.flash) / 900) : 0, pk = live ? 0.5 + 0.5 * Math.sin(now / 260) : 0, hot = hover && hover.o === a;
      ctx.globalCompositeOperation = "lighter"; glow(a.x, a.y, 36 + pk * 16 + flash * 20 + (hot ? 10 : 0), def.color, live ? .6 : .18 + flash * .4); ctx.globalCompositeOperation = "source-over";
      if (live && !reduce) {
        ctx.save(); ctx.translate(a.x, a.y); ctx.rotate(now / 1400);
        ctx.setLineDash([3, 6]); ctx.strokeStyle = def.color + "99"; ctx.lineWidth = 1.4; ctx.beginPath(); ctx.arc(0, 0, 31, 0, 7); ctx.stroke(); ctx.setLineDash([]);
        var sw = ctx.createConicGradient ? ctx.createConicGradient(0, 0, 0) : null;
        if (sw) { sw.addColorStop(0, def.color + "00"); sw.addColorStop(.12, def.color + "66"); sw.addColorStop(.13, def.color + "00"); ctx.fillStyle = sw; ctx.beginPath(); ctx.arc(0, 0, 31, 0, 7); ctx.fill(); }
        ctx.restore();
      }
      ctx.fillStyle = "#050812"; ctx.strokeStyle = def.color; ctx.lineWidth = live ? 3 : 1.8; ctx.beginPath(); ctx.arc(a.x, a.y, 20, 0, 7); ctx.fill(); ctx.stroke();
      ctx.fillStyle = def.color; ctx.font = "700 15px 'JetBrains Mono', monospace"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText(def.letter, a.x, a.y + 1);
      chip(def.label, a.x, a.y + 40, def.color, live);
    });
    // tickets: breathing glow, a ring on the ones that need a person, a glitch on the ones that failed
    ctx.globalCompositeOperation = "lighter";
    tl.forEach(function (t) {
      var col = STATE_COLOR[t.state] || "#56677f", closed = isClosed(t.state), hot = hover && hover.o === t;
      var x = t.x, y = t.y, a = t.alpha * (closed ? 0.55 : 1), br = reduce ? 0 : Math.sin(now / 520 + t.id) * .5 + .5;
      if (t.state === "failed" && !reduce) { x += (Math.random() - .5) * 1.6; if (Math.random() < .08) a *= .35; }
      ctx.globalAlpha = a;
      glow(x, y, (closed ? 9 : 15 + br * 4) + (hot ? 8 : 0), col, .55);
      ctx.fillStyle = col; ctx.beginPath(); ctx.arc(x, y, (closed ? 3 : (t.level === 3 ? 6.5 : (t.level === 2 ? 5.5 : 4.5))) + (hot ? 1.5 : 0), 0, 7); ctx.fill();
      if (t.state === "awaiting_approval" || t.state === "failed") { ctx.strokeStyle = col; ctx.lineWidth = 1.5; ctx.beginPath(); ctx.arc(x, y, 9 + (reduce ? 0 : 3 * Math.sin(now / 300)), 0, 7); ctx.stroke(); }
      ctx.globalAlpha = 1;
    });
    // ripples and sparks
    ripples = ripples.filter(function (r) { return now - r.t0 < r.dur; });
    ripples.forEach(function (r) { var k = (now - r.t0) / r.dur; ctx.strokeStyle = r.color + Math.round((1 - k) * 200).toString(16).padStart(2, "0"); ctx.lineWidth = 2 * (1 - k) + .5; ctx.beginPath(); ctx.arc(r.x, r.y, 4 + k * r.max, 0, 7); ctx.stroke(); });
    sparks = sparks.filter(function (p) { return p.age < p.life; });
    sparks.forEach(function (p) { p.age += dt; p.x += p.vx * dt; p.y += p.vy * dt; p.vx *= .96; p.vy *= .96; var f = 1 - p.age / p.life; glow(p.x, p.y, 7 * f + 2, p.color, .9 * f); });
    ctx.globalCompositeOperation = "source-over";
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
    hover = h;
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
  canvas.addEventListener("mouseleave", function () { tip.hidden = true; hover = null; });
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
