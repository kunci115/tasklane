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

  var repos = {}, tickets = {}, agents = {}, pulses = [], busyNow = {}, paused = false, prs = {};
  var PR_COLOR = { open: "#a78bfa", reviewing: "#00e5ff", approve: "#39ffb6", changes: "#ffb800", escalate: "#ff7ab6", failed: "#ff3b6b" };
  var PR_LABEL = { open: "open", reviewing: "being reviewed by the AI", approve: "AI: no changes needed", changes: "AI: changes asked", escalate: "AI: needs a person", failed: "AI review failed" };
  var core = { x: 0, y: 0, z: 40 };
  var mode = "live", replay = null;

  function hash(n) { var x = Math.sin(n * 127.1 + 311.7) * 43758.5453; return x - Math.floor(x); }
  function resize() {
    W = wrap.clientWidth; H = Math.max(520, Math.min(760, Math.round(W * 0.58)));
    canvas.style.width = W + "px"; canvas.style.height = H + "px";
    canvas.width = Math.round(W * DPR); canvas.height = Math.round(H * DPR);
    var Wu = W > 900 ? W - 330 : W; // leave room on the right for the "Working now" panel
    cx = Wu / 2; cy = H / 2; base = Math.min(Wu, H) / 940;
  }

  // ---------------------------------------------------------------- 3D world and camera
  // World units: x and y lie on the ground, z is height. The camera orbits the middle of the scene.
  var cx = 0, cy = 0, base = 1;
  var CAM0 = { yaw: 0.55, el: 0.9, zoom: 1, px: 0, py: 0 };
  var cam = { yaw: CAM0.yaw, el: CAM0.el, zoom: CAM0.zoom, px: 0, py: 0 };
  function proj(x, y, z) {
    var c1 = Math.cos(cam.yaw), s1 = Math.sin(cam.yaw), se = Math.sin(cam.el), ce = Math.cos(cam.el);
    var x1 = x * c1 - y * s1, g1 = x * s1 + y * c1;
    var sy = g1 * se - z * ce, c = g1 * ce + z * se;
    var k = base * cam.zoom * (1100 / Math.max(300, 1100 - c));
    return { x: cx + cam.px + x1 * k, y: cy + cam.py + sy * k, s: k, c: c };
  }
  function fog(p) { return Math.max(0.45, Math.min(1, 0.78 + p.c / 1500)); }

  // Things of one class sit together on one tier: the agents on the upper tier (AI agents, automation,
  // people), the repositories on the lower tier (by how much tasklane may do in them).
  var AGENT_CLASSES = [
    { name: "AGENTS · AI", color: "#00e5ff", keys: ["triage", "develop", "review"] },
    { name: "AGENTS · AUTOMATION", color: "#39ffb6", keys: ["ci", "runner"] },
    { name: "AGENTS · PEOPLE", color: "#ffb800", keys: ["you"] }
  ];
  var REPO_CLASSES = [
    { name: "REPOS · FULL", color: "#39ffb6", modes: ["full"] },
    { name: "REPOS · TRIAGE", color: "#00e5ff", modes: ["triage"] },
    { name: "REPOS · WATCH ONLY", color: "#56677f", modes: ["watch"] }
  ];
  var tiers = { agents: { r: 340, z: 150, arcs: [] }, repos: { r: 190, z: -70, arcs: [] } };
  function placeRing(groups, r, z) {
    var slots = 0, arcs = [];
    groups.forEach(function (g) { if (g.items.length) slots += g.items.length + 1; });
    if (!slots) return arcs;
    var step = Math.PI * 2 / slots, a = -Math.PI / 2;
    groups.forEach(function (g) {
      if (!g.items.length) return;
      var a0 = a;
      g.items.forEach(function (o) { o.x = Math.cos(a) * r; o.y = Math.sin(a) * r; o.z = z; a += step; });
      arcs.push({ name: g.name, color: g.color, a0: a0 - step * 0.35, a1: a - step * 0.65 });
      a += step; // an empty slot between classes
    });
    return arcs;
  }
  function layout() {
    var list = Object.keys(repos).map(function (k) { return repos[k]; });
    list.sort(function (p, q) { return String(p.name).localeCompare(String(q.name)); });
    var groups = REPO_CLASSES.map(function (c) {
      return { name: c.name, color: c.color, items: list.filter(function (r) { return c.modes.indexOf(r.mode) >= 0 || (c.modes[0] === "watch" && ["full", "triage"].indexOf(r.mode) < 0); }) };
    });
    var slots = list.length + groups.filter(function (g) { return g.items.length; }).length;
    tiers.repos.r = Math.max(170, slots * 46 / (Math.PI * 2));
    tiers.agents.r = Math.max(340, tiers.repos.r + 170);
    tiers.repos.arcs = placeRing(groups, tiers.repos.r, tiers.repos.z);
    var ag = AGENT_CLASSES.map(function (c) {
      return { name: c.name, color: c.color, items: c.keys.map(function (k) {
        var def = AGENTS.filter(function (d) { return d.key === k; })[0];
        return agents[k] || (agents[k] = { def: def, busy: 0, x: 0, y: 0, z: 0 });
      }) };
    });
    tiers.agents.arcs = placeRing(ag, tiers.agents.r, tiers.agents.z);
  }

  function setRepos(list) {
    var seen = {};
    list.forEach(function (r) {
      var o = repos[r.id] || (repos[r.id] = { id: r.id, x: 0, y: 0, z: 0 });
      o.name = r.name; o.mode = r.mode; o.open = r.open; seen[r.id] = 1;
    });
    Object.keys(repos).forEach(function (k) { if (!seen[k]) delete repos[k]; });
    layout();
  }
  function ensureTicket(id, meta) {
    var t = tickets[id];
    if (!t) {
      var r = repos[meta.repo] || { x: 0, y: 0, z: 0 };
      t = tickets[id] = { id: id, x: r.x, y: r.y, z: r.z, tx: r.x, ty: r.y, tz: r.z, born: performance.now(), alpha: 0 };
    }
    t.repo = meta.repo; t.title = meta.title; t.level = meta.level;
    return t;
  }
  // open pull requests: tied to their repository, to their ticket when tasklane opened them, and to the
  // review agent while the AI reads them
  function setPulls(list, events) {
    var keep = {};
    list.forEach(function (x) {
      var o = prs[x.key];
      if (!o) { var r = repos[x.repo] || { x: 0, y: 0, z: 0 }; o = prs[x.key] = { key: x.key, x: r.x, y: r.y, z: r.z, tx: r.x, ty: r.y, tz: r.z, alpha: 0, seed: hash(x.num * 1 + x.repo) }; }
      var changed = o.state && o.state !== x.state;
      o.repo = x.repo; o.num = x.num; o.title = x.title; o.author = x.author; o.ours = x.ours; o.ticket = x.ticket; o.url = x.url; o.state = x.state; keep[x.key] = 1;
      if (changed) ripple(o, PR_COLOR[x.state] || "#a78bfa", 40, 1000);
    });
    Object.keys(prs).forEach(function (k) { if (!keep[k]) delete prs[k]; });
    events.forEach(function (e) {
      var id = e.key + "|" + e.action + "|" + e.secs, o = prs[e.key];
      if (seenEvents[id]) return; seenEvents[id] = 1;
      if (!o) return;
      if (e.action === "pull.merged") { burst(o, "#39ffb6"); pulse(o, "runner"); } else pulse(o, "review");
    });
  }
  function prTarget(o, now) {
    var r = repos[o.repo]; if (!r) return;
    var ang = o.seed * 6.283 + (reduce ? 0 : now / 11000 * (0.4 + o.seed));
    var tk = o.ticket && tickets[o.ticket];
    if (o.state === "reviewing" && agents.review) {
      var a = agents.review, k = 0.62;
      o.tx = r.x + (a.x - r.x) * k + Math.cos(ang) * 14; o.ty = r.y + (a.y - r.y) * k + Math.sin(ang) * 14; o.tz = r.z + (a.z - r.z) * k + 6;
    } else if (tk) {
      o.tx = tk.x + Math.cos(ang) * 14; o.ty = tk.y + Math.sin(ang) * 14; o.tz = tk.z + 8;
    } else {
      var rad = 62 + o.seed * 26;
      o.tx = r.x + Math.cos(ang) * rad; o.ty = r.y + Math.sin(ang) * rad; o.tz = r.z + 12 + (o.seed - .4) * 30;
    }
  }
  function pulse(t, agentKey) {
    if (!agentKey || !agents[agentKey] || reduce) return;
    pulses.push({ id: t.id, o: t, agent: agentKey, t0: performance.now(), dur: mode === "replay" ? 700 : 1100, color: agents[agentKey].def.color });
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
      setPulls(d.pulls || [], d.pull_events || []);
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
      setRepos(d.repos); tickets = {}; prs = {}; pulses = []; busyNow = {};
      document.getElementById("mp-slider").value = 0; setPlaying(false); updateClock(); updateNow();
    });
  }
  function resetTo(vt, silent) {
    tickets = {}; prs = {}; pulses = []; replay.idx = 0; replay.vt = vt;
    while (replay.idx < replay.events.length && replay.events[replay.idx].t <= vt) { applyEvent(replay.events[replay.idx], silent); replay.idx++; }
  }
  function applyEvent(ev, silent) {
    var m = replay.meta[ev.ticket]; if (!m) return;
    var t = ensureTicket(ev.ticket, m);
    var before = t.state;
    t.state = stateAfter(ev, t.state || "new");
    if (!silent && before !== t.state) stateEffect(t, t.state);
    if (silent) { t.alpha = 1; var r = repos[m.repo]; if (r) { t.x = r.x; t.y = r.y; t.z = r.z; } return; }
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
  AGENTS.concat([{ label: "Needs you", color: "#fbbf24" }, { label: "Failed", color: "#f87171" }, { label: "Merged", color: "#34d399" }, { label: "Pull request", color: "#a78bfa" }]).forEach(function (a) {
    var s = document.createElement("span"); s.className = "lg"; var i = document.createElement("i"); i.style.background = a.color; s.appendChild(i); s.appendChild(document.createTextNode(a.label)); legend.appendChild(s);
  });

  // ---------------------------------------------------------------- effects
  var ripples = [], sparks = [], dust = [], hover = null;
  for (var di = 0; di < 70; di++) dust.push({ x: hash(di + 5), y: hash(di + 61), vx: (hash(di + 9) - .5) * .012, vy: (hash(di + 13) - .5) * .012, r: .6 + hash(di + 3) * 1.4 });
  // a ripple is a ring that spreads over the ground plane around a point of the world
  function ripple(o, color, max, dur) { if (reduce) return; ripples.push({ x: o.x, y: o.y, z: o.z || 0, color: color, max: max || 46, t0: performance.now(), dur: dur || 1100 }); }
  function burst(o, color) {
    if (reduce) return;
    for (var i = 0; i < 22; i++) { var a = Math.random() * 6.283, v = 30 + Math.random() * 90; sparks.push({ x: o.x, y: o.y, z: o.z || 0, vx: Math.cos(a) * v, vy: Math.sin(a) * v, vz: (Math.random() - .3) * 90, life: 0.7 + Math.random() * 0.6, age: 0, color: color }); }
    ripple(o, color, 70, 1300);
  }
  // effects that follow a ticket's change of state
  function stateEffect(t, state) {
    var c = STATE_COLOR[state] || "#38bdf8";
    if (state === "merged") burst(t, "#39ffb6");
    else if (state === "failed") { ripple(t, "#ff3b6b", 60, 1200); setTimeout(function () { ripple(t, "#ff3b6b", 60, 1200); }, 260); }
    else if (state === "awaiting_approval") ripple(t, "#ffb800", 56, 1300);
    else ripple(t, c, 38, 900);
  }

  // ---------------------------------------------------------------- drawing
  function chip(text, x, y, color, strong, alpha) {
    ctx.font = (strong ? "700 " : "500 ") + "12px 'JetBrains Mono', ui-monospace, Menlo, monospace";
    var w = ctx.measureText(text).width + 16, h = 22, r = 11;
    taken.push([x - w / 2, y - h / 2, x + w / 2, y + h / 2]);
    ctx.globalAlpha = alpha === undefined ? 1 : alpha;
    ctx.fillStyle = "rgba(4,7,14,.86)"; ctx.strokeStyle = color; ctx.lineWidth = 1.2;
    ctx.beginPath(); ctx.moveTo(x - w / 2 + r, y - h / 2); ctx.arcTo(x + w / 2, y - h / 2, x + w / 2, y + h / 2, r); ctx.arcTo(x + w / 2, y + h / 2, x - w / 2, y + h / 2, r);
    ctx.arcTo(x - w / 2, y + h / 2, x - w / 2, y - h / 2, r); ctx.arcTo(x - w / 2, y - h / 2, x + w / 2, y - h / 2, r); ctx.closePath(); ctx.fill(); ctx.stroke();
    ctx.fillStyle = "#e6f4ff"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText(text, x, y + 1);
    ctx.globalAlpha = 1;
  }
  // Class names are drawn after everything else and only where nothing is in the way, so they
  // never sit half behind a chip or a node. `taken` holds the boxes the frame has used.
  var taken = [], pendingTags = [];
  function tag(text, x, y, color) { pendingTags.push([text, x, y, color]); }
  function drawTags() {
    ctx.font = "700 10px 'JetBrains Mono', ui-monospace, Menlo, monospace"; ctx.textAlign = "center"; ctx.textBaseline = "middle";
    pendingTags.forEach(function (t) {
      var w = t[0].length * 8.2 + 8, box = [t[1] - w / 2, t[2] - 8, t[1] + w / 2, t[2] + 8];
      var clash = taken.some(function (b) { return box[0] < b[2] && box[2] > b[0] && box[1] < b[3] && box[3] > b[1]; });
      if (clash) return;
      taken.push(box);
      ctx.fillStyle = t[3]; ctx.globalAlpha = .9;
      if ("letterSpacing" in ctx) { ctx.letterSpacing = "2px"; ctx.fillText(t[0], t[1], t[2]); ctx.letterSpacing = "0px"; } else ctx.fillText(t[0], t[1], t[2]);
      ctx.globalAlpha = 1;
    });
  }
  function glow(x, y, r, color, a) {
    var g = ctx.createRadialGradient(x, y, 0, x, y, r); g.addColorStop(0, color + Math.round(Math.max(0, Math.min(1, a)) * 255).toString(16).padStart(2, "0")); g.addColorStop(1, color + "00");
    ctx.fillStyle = g; ctx.beginPath(); ctx.arc(x, y, r, 0, 7); ctx.fill();
  }
  // a circle lying on the ground plane (an ellipse on the screen)
  function ringPath(x, y, z, r, from, to) {
    var n = Math.max(8, Math.round(64 * (to - from) / 6.2832));
    for (var i = 0; i <= n; i++) { var a = from + (to - from) * i / n, p = proj(x + Math.cos(a) * r, y + Math.sin(a) * r, z); if (i) ctx.lineTo(p.x, p.y); else ctx.moveTo(p.x, p.y); }
  }
  function ticketTarget(t, now) {
    var r = repos[t.repo]; if (!r) return;
    var ag = isClosed(t.state) ? null : agentFor(t.state);
    var ang = hash(t.id) * 6.283 + (reduce ? 0 : now / 9000 * (0.4 + hash(t.id + 3)));
    if (ag && agents[ag]) {
      var a = agents[ag], k = 0.62;
      t.tx = r.x + (a.x - r.x) * k + Math.cos(ang) * 16; t.ty = r.y + (a.y - r.y) * k + Math.sin(ang) * 16; t.tz = r.z + (a.z - r.z) * k + Math.sin(ang * 1.7) * 10;
    } else {
      var rad = (isClosed(t.state) ? 70 : 38) + hash(t.id + 7) * 28;
      t.tx = r.x + Math.cos(ang) * rad; t.ty = r.y + Math.sin(ang) * rad; t.tz = r.z + (hash(t.id + 9) - .3) * (isClosed(t.state) ? 70 : 46);
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
    // drifting dust, far behind everything
    ctx.fillStyle = "rgba(120,235,255,.30)";
    dust.forEach(function (d) { if (!reduce) { d.x += d.vx * dt; d.y += d.vy * dt; if (d.x < 0) d.x += 1; if (d.x > 1) d.x -= 1; if (d.y < 0) d.y += 1; if (d.y > 1) d.y -= 1; } ctx.fillRect(d.x * W, d.y * H, d.r, d.r); });

    // ground grid
    var G = 560, FLOOR = -170;
    ctx.lineWidth = 1;
    for (var gi = -G; gi <= G; gi += 80) {
      var e = Math.abs(gi) / G; ctx.strokeStyle = "rgba(0,229,255," + (0.085 * (1 - e * 0.7)).toFixed(3) + ")";
      var p1 = proj(gi, -G, FLOOR), p2 = proj(gi, G, FLOOR); ctx.beginPath(); ctx.moveTo(p1.x, p1.y); ctx.lineTo(p2.x, p2.y); ctx.stroke();
      p1 = proj(-G, gi, FLOOR); p2 = proj(G, gi, FLOOR); ctx.beginPath(); ctx.moveTo(p1.x, p1.y); ctx.lineTo(p2.x, p2.y); ctx.stroke();
    }
    taken.length = 0; pendingTags.length = 0;
    // the two tiers: a faint plate, its rim, and an arc and a name for each class
    [["agents", "AGENTS", "#00e5ff"], ["repos", "REPOSITORIES", "#39ffb6"]].forEach(function (td) {
      var T = tiers[td[0]], c = proj(0, 0, T.z);
      ctx.beginPath(); ringPath(0, 0, T.z, T.r + 40, 0, 6.2832); ctx.closePath();
      ctx.fillStyle = td[2] + "0d"; ctx.fill(); ctx.strokeStyle = td[2] + "40"; ctx.lineWidth = 1; ctx.stroke();
      ctx.setLineDash([2, 6]); ctx.beginPath(); ringPath(0, 0, T.z, T.r - 40, 0, 6.2832); ctx.strokeStyle = td[2] + "22"; ctx.stroke(); ctx.setLineDash([]);
      T.arcs.forEach(function (A) {
        ctx.beginPath(); ringPath(0, 0, T.z, T.r + 40, A.a0, A.a1); ctx.strokeStyle = A.color; ctx.lineWidth = 3; ctx.globalAlpha = .85; ctx.stroke(); ctx.globalAlpha = 1;
        var m = (A.a0 + A.a1) / 2, lp = proj(Math.cos(m) * (T.r + 80), Math.sin(m) * (T.r + 80), T.z);
        tag(A.name, lp.x, lp.y, A.color);
      });
          });
    // the axis that ties the tiers to the core
    var ax0 = proj(0, 0, tiers.repos.z), ax1 = proj(0, 0, tiers.agents.z);
    ctx.strokeStyle = "rgba(255,46,147,.25)"; ctx.setLineDash([3, 5]); ctx.beginPath(); ctx.moveTo(ax0.x, ax0.y); ctx.lineTo(ax1.x, ax1.y); ctx.stroke(); ctx.setLineDash([]);

    var tl = Object.keys(tickets).map(function (k) { return tickets[k]; });
    // move tickets first so every later layer uses this frame's positions
    tl.forEach(function (t) {
      ticketTarget(t, now); var k = Math.min(1, dt * 3);
      t.x += (t.tx - t.x) * k; t.y += (t.ty - t.y) * k; t.z += (t.tz - t.z) * k;
      t.alpha = Math.min(1, t.alpha + dt * 2);
      if (t.fx === undefined) { t.fx = 1; if (t.alpha < 1 && t.state && !isClosed(t.state)) ripple(t, STATE_COLOR[t.state] || "#38bdf8", 40, 1000); }
      t.P = proj(t.x, t.y, t.z);
    });
    var pl = Object.keys(prs).map(function (k) { return prs[k]; });
    pl.forEach(function (o) {
      prTarget(o, now); var k = Math.min(1, dt * 3);
      o.x += (o.tx - o.x) * k; o.y += (o.ty - o.y) * k; o.z += (o.tz - o.z) * k; o.alpha = Math.min(1, o.alpha + dt * 2);
      o.P = proj(o.x, o.y, o.z);
    });
    var repoList = Object.keys(repos).map(function (k) { return repos[k]; });
    repoList.forEach(function (r) { r.P = proj(r.x, r.y, r.z); });
    AGENTS.forEach(function (d) { var a = agents[d.key]; if (a) a.P = proj(a.x, a.y, a.z); });
    core.P = proj(core.x, core.y, core.z);

    // links: core -> repos
    ctx.lineWidth = 1;
    repoList.forEach(function (r) { ctx.strokeStyle = "rgba(0,229,255,.18)"; ctx.beginPath(); ctx.moveTo(core.P.x, core.P.y); ctx.lineTo(r.P.x, r.P.y); ctx.stroke(); });
    // repo -> ticket threads, and the flowing data line from each assigned ticket to its agent
    tl.forEach(function (t) {
      var r = repos[t.repo], ag = isClosed(t.state) ? null : agentFor(t.state);
      if (r && r.P) { ctx.strokeStyle = "rgba(0,229,255,.08)"; ctx.beginPath(); ctx.moveTo(r.P.x, r.P.y); ctx.lineTo(t.P.x, t.P.y); ctx.stroke(); }
      if (ag && agents[ag]) {
        var A = agents[ag], col = A.def.color, hot = hover && (hover.o === t || hover.o === A);
        ctx.strokeStyle = col + (hot ? "cc" : "55"); ctx.lineWidth = hot ? 1.8 : 1.2;
        ctx.setLineDash([5, 7]); ctx.lineDashOffset = reduce ? 0 : -now / 40;
        ctx.beginPath(); ctx.moveTo(t.P.x, t.P.y); ctx.lineTo(A.P.x, A.P.y); ctx.stroke(); ctx.setLineDash([]); ctx.lineWidth = 1;
        if (!reduce) { // a bead of light travelling along the line
          var k = ((now / 1700) + hash(t.id)) % 1, px = t.P.x + (A.P.x - t.P.x) * k, py = t.P.y + (A.P.y - t.P.y) * k;
          ctx.globalCompositeOperation = "lighter"; glow(px, py, 9, col, .85); ctx.globalCompositeOperation = "source-over";
        }
      }
    });
    // repo -> pull request threads; a pull request tasklane opened is tied to its ticket; one the AI is
    // reading is tied to the review agent with a flowing line, like a ticket in someone's hands
    pl.forEach(function (o) {
      var r = repos[o.repo], tk = o.ticket && tickets[o.ticket], col = PR_COLOR[o.state] || "#a78bfa";
      if (r && r.P) { ctx.strokeStyle = "rgba(167,139,250,.14)"; ctx.beginPath(); ctx.moveTo(r.P.x, r.P.y); ctx.lineTo(o.P.x, o.P.y); ctx.stroke(); }
      if (tk && tk.P) { ctx.strokeStyle = "rgba(255,184,0,.35)"; ctx.setLineDash([2, 4]); ctx.beginPath(); ctx.moveTo(tk.P.x, tk.P.y); ctx.lineTo(o.P.x, o.P.y); ctx.stroke(); ctx.setLineDash([]); }
      if (o.state === "reviewing" && agents.review && agents.review.P) {
        var A = agents.review, hot = hover && (hover.o === o || hover.o === A);
        ctx.strokeStyle = A.def.color + (hot ? "cc" : "88"); ctx.lineWidth = hot ? 1.8 : 1.4; ctx.setLineDash([5, 7]); ctx.lineDashOffset = reduce ? 0 : -now / 40;
        ctx.beginPath(); ctx.moveTo(o.P.x, o.P.y); ctx.lineTo(A.P.x, A.P.y); ctx.stroke(); ctx.setLineDash([]); ctx.lineWidth = 1;
        if (!reduce) { var kk = ((now / 1500) + o.seed) % 1, px = o.P.x + (A.P.x - o.P.x) * kk, py = o.P.y + (A.P.y - o.P.y) * kk; ctx.globalCompositeOperation = "lighter"; glow(px, py, 9, A.def.color, .9); ctx.globalCompositeOperation = "source-over"; }
      }
    });
    // pulses leave a comet tail
    var ended = [];
    pulses = pulses.filter(function (p) { var live = now - p.t0 < p.dur; if (!live) ended.push(p); return live; });
    ended.forEach(function (p) { var a = agents[p.agent]; if (a) ripple(a, p.color, 52, 900); });
    ctx.globalCompositeOperation = "lighter";
    pulses.forEach(function (p) {
      var t = p.o || tickets[p.id], a = agents[p.agent]; if (!t || !a) return;
      var k = (now - p.t0) / p.dur, q = proj(t.x + (a.x - t.x) * k, t.y + (a.y - t.y) * k, t.z + (a.z - t.z) * k), x = q.x, y = q.y;
      p.trail = p.trail || []; p.trail.push([x, y]); if (p.trail.length > 16) p.trail.shift();
      for (var i = 0; i < p.trail.length; i++) { var f = i / p.trail.length; glow(p.trail[i][0], p.trail[i][1], 4 + 9 * f, p.color, .55 * f); }
      glow(x, y, 20, p.color, .95); ctx.fillStyle = "#fff"; ctx.beginPath(); ctx.arc(x, y, 2.8, 0, 7); ctx.fill();
    });
    ctx.globalCompositeOperation = "source-over";

    // everything that has a place in the world, drawn far to near
    var items = [];
    // the core: heartbeat rings spread over the ground, then the disc
    items.push({ c: core.P.c, f: function () {
      var P = core.P, u = P.s;
      if (!reduce) for (var h = 0; h < 2; h++) { var hk = ((now / 3200) + h * .5) % 1; ctx.strokeStyle = "rgba(255,46,147," + (0.34 * (1 - hk)).toFixed(3) + ")"; ctx.lineWidth = 1.5; ctx.beginPath(); ringPath(core.x, core.y, core.z, 20 + hk * 90, 0, 6.2832); ctx.stroke(); }
      ctx.globalCompositeOperation = "lighter"; glow(P.x, P.y, 90 * u, "#ff2e93", .42); ctx.globalCompositeOperation = "source-over";
      ctx.fillStyle = "#ff2e93"; ctx.beginPath(); ctx.arc(P.x, P.y, 19 * u, 0, 7); ctx.fill();
      ctx.strokeStyle = "rgba(255,255,255,.35)"; ctx.lineWidth = 1; ctx.beginPath(); ctx.arc(P.x, P.y, 24 * u, 0, 7); ctx.stroke();
      ctx.fillStyle = "#1a0410"; ctx.font = "700 " + Math.max(10, 16 * u) + "px 'JetBrains Mono', monospace"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText("t", P.x, P.y + 1);
      ctx.fillStyle = "rgba(230,244,255,.7)"; ctx.font = "500 11px 'JetBrains Mono', ui-monospace, monospace"; ctx.fillText(paused && mode === "live" ? "tasklane · paused" : "tasklane", P.x, P.y + 40 * u);
    } });
    // repositories: a ring that fills with how much is open
    var showNames = repoList.length <= 10 || cam.zoom > 1.5;
    repoList.forEach(function (r) {
      items.push({ c: r.P.c, f: function () {
        var P = r.P, u = P.s, F = fog(P), col = r.mode === "full" ? "#39ffb6" : (r.mode === "triage" ? "#00e5ff" : "#56677f"), hot = hover && hover.o === r;
        ctx.globalAlpha = F;
        ctx.globalCompositeOperation = "lighter"; glow(P.x, P.y, (hot ? 58 : 44) * u, col, hot ? .4 : .26); ctx.globalCompositeOperation = "source-over";
        taken.push([P.x - 24 * u, P.y - 24 * u, P.x + 24 * u, P.y + 24 * u]);
        ctx.fillStyle = "#050812"; ctx.strokeStyle = col; ctx.lineWidth = 2; ctx.beginPath(); ctx.arc(P.x, P.y, 16 * u, 0, 7); ctx.fill(); ctx.stroke();
        var frac = Math.min(1, (r.open || 0) / 10);
        if (frac > 0) { ctx.strokeStyle = col; ctx.lineWidth = 3; ctx.lineCap = "round"; ctx.beginPath(); ctx.arc(P.x, P.y, 23 * u, -Math.PI / 2, -Math.PI / 2 + frac * 6.283); ctx.stroke(); ctx.lineCap = "butt"; }
        ctx.globalAlpha = 1;
        if (showNames || hot) chip(r.name.split("/").pop(), P.x, P.y - 34 * u, col, true, F);
      } });
    });
    // agents: busy ones glow, spin a dashed orbit and sweep like a radar
    AGENTS.forEach(function (def) {
      var a = agents[def.key]; if (!a || !a.P) return;
      items.push({ c: a.P.c, f: function () {
        var P = a.P, u = P.s, F = fog(P);
        var live = mode === "live" ? (busyNow[def.key] || []).length > 0 : (a.replayUntil || 0) > now;
        var flash = a.flash ? Math.max(0, 1 - (now - a.flash) / 900) : 0, pk = live ? 0.5 + 0.5 * Math.sin(now / 260) : 0, hot = hover && hover.o === a;
        ctx.globalAlpha = F;
        ctx.globalCompositeOperation = "lighter"; glow(P.x, P.y, (40 + pk * 18 + flash * 22 + (hot ? 10 : 0)) * u, def.color, live ? .6 : .18 + flash * .4); ctx.globalCompositeOperation = "source-over";
        if (live && !reduce) {
          ctx.setLineDash([3, 6]); ctx.lineDashOffset = -now / 60; ctx.strokeStyle = def.color + "99"; ctx.lineWidth = 1.4; ctx.beginPath(); ringPath(a.x, a.y, a.z, 34, 0, 6.2832); ctx.stroke(); ctx.setLineDash([]);
          ctx.save(); ctx.translate(P.x, P.y); ctx.rotate(now / 1400);
          var sw = ctx.createConicGradient ? ctx.createConicGradient(0, 0, 0) : null;
          if (sw) { sw.addColorStop(0, def.color + "00"); sw.addColorStop(.12, def.color + "66"); sw.addColorStop(.13, def.color + "00"); ctx.fillStyle = sw; ctx.beginPath(); ctx.arc(0, 0, 34 * u, 0, 7); ctx.fill(); }
          ctx.restore();
        }
        taken.push([P.x - 30 * u, P.y - 30 * u, P.x + 30 * u, P.y + 30 * u]);
        ctx.fillStyle = "#050812"; ctx.strokeStyle = def.color; ctx.lineWidth = live ? 3 : 1.8; ctx.beginPath(); ctx.arc(P.x, P.y, 22 * u, 0, 7); ctx.fill(); ctx.stroke();
        ctx.fillStyle = def.color; ctx.font = "700 " + Math.max(10, 16 * u) + "px 'JetBrains Mono', monospace"; ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText(def.letter, P.x, P.y + 1);
        ctx.globalAlpha = 1;
        chip(def.label, P.x, P.y + 42 * u, def.color, live, F);
      } });
    });
    // tickets: breathing glow, a ring on the ones that need a person, a glitch on the ones that failed
    tl.forEach(function (t) {
      items.push({ c: t.P.c, f: function () {
        var P = t.P, u = P.s, col = STATE_COLOR[t.state] || "#56677f", closed = isClosed(t.state), hot = hover && hover.o === t;
        var x = P.x, y = P.y, a = t.alpha * (closed ? 0.55 : 1) * fog(P), br = reduce ? 0 : Math.sin(now / 520 + t.id) * .5 + .5;
        if (t.state === "failed" && !reduce) { x += (Math.random() - .5) * 1.6; if (Math.random() < .08) a *= .35; }
        ctx.globalCompositeOperation = "lighter"; ctx.globalAlpha = a;
        glow(x, y, ((closed ? 11 : 18 + br * 5) + (hot ? 9 : 0)) * u, col, .55);
        ctx.fillStyle = col; ctx.beginPath(); ctx.arc(x, y, ((closed ? 3.4 : (t.level === 3 ? 7.5 : (t.level === 2 ? 6.3 : 5.2))) + (hot ? 1.6 : 0)) * u, 0, 7); ctx.fill();
        if (t.state === "awaiting_approval" || t.state === "failed") { ctx.strokeStyle = col; ctx.lineWidth = 1.5; ctx.beginPath(); ctx.arc(x, y, (10 + (reduce ? 0 : 3 * Math.sin(now / 300))) * u, 0, 7); ctx.stroke(); }
        ctx.globalAlpha = 1; ctx.globalCompositeOperation = "source-over";
      } });
    });
    // open pull requests: a diamond, violet until the AI has looked, then coloured by what it said
    pl.forEach(function (o) {
      items.push({ c: o.P.c, f: function () {
        var P = o.P, u = P.s, col = PR_COLOR[o.state] || "#a78bfa", hot = hover && hover.o === o, a = o.alpha * fog(P), br = reduce ? 0 : Math.sin(now / 420 + o.seed * 9) * .5 + .5;
        var rr = (7 + (o.state === "reviewing" ? br * 2.5 : 0) + (hot ? 2 : 0)) * u;
        ctx.globalAlpha = a; ctx.globalCompositeOperation = "lighter"; glow(P.x, P.y, (o.state === "reviewing" ? 26 + br * 8 : 17) * u, col, .5); ctx.globalCompositeOperation = "source-over";
        taken.push([P.x - rr, P.y - rr, P.x + rr, P.y + rr]);
        ctx.fillStyle = "#050812"; ctx.strokeStyle = col; ctx.lineWidth = 1.8; ctx.beginPath(); ctx.moveTo(P.x, P.y - rr); ctx.lineTo(P.x + rr, P.y); ctx.lineTo(P.x, P.y + rr); ctx.lineTo(P.x - rr, P.y); ctx.closePath(); ctx.fill(); ctx.stroke();
        if (o.ours) { ctx.fillStyle = col; ctx.beginPath(); ctx.arc(P.x, P.y, 1.8 * u, 0, 7); ctx.fill(); }
        ctx.globalAlpha = 1;
        if (hot || cam.zoom > 1.9) chip("#" + o.num, P.x, P.y - 18 * u, col, hot, a);
      } });
    });
    items.sort(function (p, q) { return p.c - q.c; }).forEach(function (it) { it.f(); });
    drawTags();

    // ripples spread over the ground; sparks fly in space
    ctx.globalCompositeOperation = "lighter";
    ripples = ripples.filter(function (r) { return now - r.t0 < r.dur; });
    ripples.forEach(function (r) { var k = (now - r.t0) / r.dur; ctx.strokeStyle = r.color + Math.round((1 - k) * 200).toString(16).padStart(2, "0"); ctx.lineWidth = 2 * (1 - k) + .5; ctx.beginPath(); ringPath(r.x, r.y, r.z, 4 + k * r.max, 0, 6.2832); ctx.stroke(); });
    sparks = sparks.filter(function (p) { return p.age < p.life; });
    sparks.forEach(function (p) { p.age += dt; p.x += p.vx * dt; p.y += p.vy * dt; p.z += p.vz * dt; p.vx *= .96; p.vy *= .96; p.vz *= .96; var f = 1 - p.age / p.life, q = proj(p.x, p.y, p.z); glow(q.x, q.y, (7 * f + 2) * Math.max(.6, q.s * 1.2), p.color, .9 * f); });
    ctx.globalCompositeOperation = "source-over";
    // a scan line sweeping down the screen
    if (!reduce) { var sy = ((now / 6500) % 1) * (H + 80) - 40, sg = ctx.createLinearGradient(0, sy - 40, 0, sy + 40); sg.addColorStop(0, "rgba(0,229,255,0)"); sg.addColorStop(.5, "rgba(0,229,255,.06)"); sg.addColorStop(1, "rgba(0,229,255,0)"); ctx.fillStyle = sg; ctx.fillRect(0, sy - 40, W, 80); }
    requestAnimationFrame(frame);
  }

  // ---------------------------------------------------------------- pointer: orbit, pan, zoom, pinch
  function hit(mx, my) {
    var best = null, bc = -1e9;
    function consider(type, o, rad) { if (!o.P) return; var d = Math.hypot(o.P.x - mx, o.P.y - my); if (d < Math.max(11, rad * o.P.s) && o.P.c > bc) { bc = o.P.c; best = { type: type, o: o }; } }
    Object.keys(tickets).forEach(function (k) { consider("ticket", tickets[k], 8); });
    Object.keys(prs).forEach(function (k) { consider("pull", prs[k], 9); });
    if (best) return best;
    AGENTS.forEach(function (def) { var a = agents[def.key]; if (a) consider("agent", a, 24); });
    Object.keys(repos).forEach(function (k) { consider("repo", repos[k], 18); });
    return best;
  }
  function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }
  function zoomAt(mx, my, factor) {
    var z = clamp(cam.zoom * factor, 0.35, 5), ratio = z / cam.zoom;
    cam.px = (mx - cx) - ((mx - cx) - cam.px) * ratio; cam.py = (my - cy) - ((my - cy) - cam.py) * ratio; cam.zoom = z;
  }
  function showTip(h, mx, my) {
    if (!h) { tip.hidden = true; return; }
    while (tip.firstChild) tip.removeChild(tip.firstChild);
    var t1 = document.createElement("strong"), t2 = document.createElement("div");
    if (h.type === "ticket") { t1.textContent = h.o.title; t2.textContent = (STATE_LABEL[h.o.state] || h.o.state) + (h.o.level ? " · level " + h.o.level : ""); }
    else if (h.type === "pull") { t1.textContent = "#" + h.o.num + " " + h.o.title; t2.textContent = (h.o.ours ? "opened by tasklane" : "by " + h.o.author) + " · " + (PR_LABEL[h.o.state] || h.o.state) + " · click to open on GitHub"; }
    else if (h.type === "agent") { t1.textContent = h.o.def.label; var bl = (busyNow[h.o.def.key] || []); t2.textContent = mode === "live" ? (bl.length ? "Working on: " + bl.map(function (x) { return x.title; }).join(", ") : "Idle") : "Replay"; }
    else { t1.textContent = h.o.name; t2.textContent = "Mode: " + h.o.mode + (h.o.open ? " · " + h.o.open + " open" : ""); }
    tip.appendChild(t1); tip.appendChild(t2); tip.hidden = false;
    tip.style.left = Math.min(W - 250, mx + 14) + "px"; tip.style.top = (my + 14) + "px";
  }
  var ptrs = {}, moved = 0, pinchD = 0;
  function count() { return Object.keys(ptrs).length; }
  function pinchDist() { var k = Object.keys(ptrs); return Math.hypot(ptrs[k[0]].x - ptrs[k[1]].x, ptrs[k[0]].y - ptrs[k[1]].y); }
  canvas.style.touchAction = "none";
  canvas.addEventListener("contextmenu", function (e) { e.preventDefault(); });
  canvas.addEventListener("pointerdown", function (e) {
    try { canvas.setPointerCapture(e.pointerId); } catch (x) {}
    ptrs[e.pointerId] = { x: e.clientX, y: e.clientY }; if (count() === 1) moved = 0; if (count() === 2) pinchD = pinchDist();
    tip.hidden = true;
  });
  canvas.addEventListener("pointermove", function (e) {
    var b = canvas.getBoundingClientRect(), mx = e.clientX - b.left, my = e.clientY - b.top, p = ptrs[e.pointerId];
    if (p) {
      var dx = e.clientX - p.x, dy = e.clientY - p.y; p.x = e.clientX; p.y = e.clientY; moved += Math.abs(dx) + Math.abs(dy);
      if (count() === 1) {
        if (e.shiftKey || (e.buttons & 6)) { cam.px += dx; cam.py += dy; }
        else { cam.yaw += dx * 0.008; cam.el = clamp(cam.el + dy * 0.006, 0.2, 1.5); }
        canvas.style.cursor = "grabbing";
      } else if (count() === 2) {
        var d = pinchDist(); if (pinchD > 0) zoomAt(mx, my, d / pinchD); pinchD = d; cam.px += dx / 2; cam.py += dy / 2;
      }
      return;
    }
    hover = hit(mx, my); canvas.style.cursor = hover && (hover.type === "ticket" || hover.type === "pull") ? "pointer" : "grab"; showTip(hover, mx, my);
  });
  function release(e) {
    var b = canvas.getBoundingClientRect(), was = count();
    if (ptrs[e.pointerId]) delete ptrs[e.pointerId];
    if (was === 1 && e.type === "pointerup" && moved < 6) {
      var h = hit(e.clientX - b.left, e.clientY - b.top);
      if (h && h.type === "ticket") location.href = "/tickets/" + h.o.id;
      else if (h && h.type === "pull" && /^https:\/\//.test(h.o.url || "")) window.open(h.o.url, "_blank", "noopener");
    }
    canvas.style.cursor = "grab";
  }
  canvas.addEventListener("pointerup", release);
  canvas.addEventListener("pointercancel", release);
  canvas.addEventListener("mouseleave", function () { tip.hidden = true; hover = null; });
  canvas.addEventListener("wheel", function (e) {
    e.preventDefault(); var b = canvas.getBoundingClientRect();
    zoomAt(e.clientX - b.left, e.clientY - b.top, Math.exp(-e.deltaY * (e.ctrlKey ? 0.01 : 0.0015)));
  }, { passive: false });
  canvas.addEventListener("dblclick", function () { cam.yaw = CAM0.yaw; cam.el = CAM0.el; cam.zoom = 1; cam.px = 0; cam.py = 0; });
  document.querySelectorAll("[data-map-cam]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      var a = btn.getAttribute("data-map-cam");
      if (a === "in") zoomAt(cx, cy, 1.25); else if (a === "out") zoomAt(cx, cy, 0.8);
      else if (a === "left") cam.yaw -= 0.3; else if (a === "right") cam.yaw += 0.3;
      else if (a === "top") cam.el = cam.el > 1.3 ? CAM0.el : 1.5;
      else { cam.yaw = CAM0.yaw; cam.el = CAM0.el; cam.zoom = 1; cam.px = 0; cam.py = 0; }
    });
  });

  // ---------------------------------------------------------------- controls
  document.querySelectorAll("[data-map-mode]").forEach(function (btn) {
    btn.addEventListener("click", function () {
      mode = btn.getAttribute("data-map-mode");
      document.querySelectorAll("[data-map-mode]").forEach(function (x) { x.setAttribute("aria-pressed", x === btn ? "true" : "false"); });
      bar.hidden = mode !== "replay"; tickets = {}; prs = {}; pulses = []; busyNow = {};
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
  layout(); resize(); pollLive(); setInterval(pollLive, 3000); requestAnimationFrame(frame);
  var rep = document.getElementById("mp-speed"); if (rep) rep.value = "60";
})();
