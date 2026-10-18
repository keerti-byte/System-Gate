// System Gate frontend. No build step: Tauri injects window.__TAURI__ (withGlobalTauri).
// Rust does all privileged work and all decisions (including when the Gate seals);
// this file renders state, plays animations/sounds (fx.js, sounds.js) and forwards clicks over IPC.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (sel) => document.querySelector(sel);
const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let config = null;                // last config from Rust
let status = null;                // last snapshot from Rust
let busy = false;                 // a long action is running
const completing = new Set();     // quest keys whose tick is in flight
const ghosts = new Map();         // just-completed quests, kept on screen for the exit animation
const knownQuests = new Set();    // quest keys already shown (only new ones get the entrance animation)
let prevMissed = null;            // keys that were missed at the last snapshot (to warn on new ones)
let prevPhase = null;             // timer phase at the last snapshot (to ring the bell)

let prev = { rank: null, level: null, exp: null };
let pendingEvolution = null;      // rank change that happened while the window was hidden

/* ---------- helpers ---------- */

// 12-hour clock everywhere.
const TIME_OPTS = { hour: "numeric", minute: "2-digit", hour12: true };
const fmtTime = (ts) => new Date(ts * 1000).toLocaleTimeString([], TIME_OPTS);
const fmtMin = (m) => new Date(2000, 0, 1, Math.floor(m / 60), m % 60).toLocaleTimeString([], TIME_OPTS);
const fmtDate = (iso) => {
  const [y, m, d] = iso.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
};
const nowSecs = () => Date.now() / 1000;
const todayStr = () => new Date().toLocaleDateString("en-CA"); // YYYY-MM-DD, local
const tomorrowStr = () => { const d = new Date(); d.setDate(d.getDate() + 1); return d.toLocaleDateString("en-CA"); };
const pad = (n) => String(n).padStart(2, "0");
const hms = (secs) => {
  const s = Math.max(0, Math.floor(secs));
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor((s % 3600) / 60))}:${pad(s % 60)}`;
};
const ms = (secs) => `${pad(Math.floor(Math.max(0, secs) / 60))}:${pad(Math.floor(Math.max(0, secs) % 60))}`;
const fmtFocus = (secs) => {
  const m = Math.round(secs / 60);
  return m >= 60 ? `${Math.floor(m / 60)} h ${m % 60} min` : `${m} min`;
};
const hhmmToMin = (v) => { if (!v) return null; const [h, m] = v.split(":").map(Number); return h * 60 + m; };
const minToHhmm = (m) => `${pad(Math.floor(m / 60))}:${pad(m % 60)}`;

/* A 12-hour time picker (hour, minute, AM/PM). The native <input type="time"> follows the OS
   locale and can show 24-hour time, so this is built by hand to always be 12-hour. */
function initTime12(root, optional) {
  root.replaceChildren();
  const mk = (label, values, cls) => {
    const s = el("select", cls);
    s.setAttribute("aria-label", `${label} ${root.id === "rs-time" ? "start" : "lock"} time`);
    for (const [v, t] of values) { const o = el("option", "", t); o.value = v; s.append(o); }
    return s;
  };
  const hours = [...(optional ? [["", "--"]] : []), ...Array.from({ length: 12 }, (_, i) => [String(i + 1), String(i + 1)])];
  const mins = Array.from({ length: 12 }, (_, i) => [String(i * 5), pad(i * 5)]);
  const h = mk("Hour", hours, "t-h"), mi = mk("Minute", mins, "t-m"), ap = mk("AM or PM", [["AM", "AM"], ["PM", "PM"]], "t-ap");
  h.addEventListener("change", () => { mi.disabled = ap.disabled = h.value === ""; });
  root.append(h, el("span", "t-sep", ":"), mi, ap);
}
/** minutes after midnight (or null = no time) -> the picker. Minutes snap to 5. */
function setTime12(root, minutes) {
  const [h, mi, ap] = root.querySelectorAll("select");
  if (minutes == null) { h.value = ""; mi.disabled = ap.disabled = true; return; }
  const hr = Math.floor(minutes / 60) % 24;
  h.value = String(hr % 12 === 0 ? 12 : hr % 12);
  mi.value = String(Math.round((minutes % 60) / 5) * 5 % 60);
  ap.value = hr >= 12 ? "PM" : "AM";
  mi.disabled = ap.disabled = false;
}
function getTime12(root) {
  const [h, mi, ap] = root.querySelectorAll("select");
  if (h.value === "") return null;
  return ((Number(h.value) % 12) + (ap.value === "PM" ? 12 : 0)) * 60 + Number(mi.value);
}

function el(tag, className, text) {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text; // textContent only: never inject HTML
  return e;
}

function animateNumber(node, to, format = (v) => String(Math.round(v))) {
  const from = node.dataset.v === undefined ? to : Number(node.dataset.v);
  node.dataset.v = String(to);
  FX.tween(from, to, 700, (v) => (node.textContent = format(v)));
}

function setRing(sel, pct) {
  const c = $(sel);
  const circ = 2 * Math.PI * Number(c.getAttribute("r"));
  c.style.strokeDasharray = String(circ);
  c.style.strokeDashoffset = String(circ * (1 - Math.min(1, Math.max(0, pct / 100))));
}

/** System-style NOTIFICATION window. */
function toast(message, kind = "info", ms = 5000) {
  const t = el("div", `toast ${kind === "bad" ? "bad" : ""}`);
  const head = el("div", "notif-head");
  head.append(el("span", "icon-box", "!"), el("h2", "boxed", "Notification"));
  t.append(head, el("p", "", message));
  $("#toasts").append(t);
  if (kind === "bad") Sfx.play("error");
  setTimeout(() => t.remove(), ms);
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (err) {
    toast(String(err), "bad", 9000);
    throw err;
  }
}

function setBusy(on) {
  busy = on;
  renderStatus();
}

// a soft click on buttons and tabs
document.addEventListener("click", (e) => {
  if (e.target.closest(".btn:not(#btn-test-sound), .tabs button, .actbtn")) Sfx.play("tick");
});

/* ---------- rank colour (Color Evolution) ---------- */

function applyTheme(hex) {
  const root = document.documentElement;
  if (root.dataset.neon === hex) return;
  const n = parseInt(hex.slice(1), 16);
  const rgb = `${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}`;
  root.style.setProperty("--neon", hex);
  root.style.setProperty("--neon-rgb", rgb);
  root.dataset.neon = hex;
  FX.setColor(rgb);
}

const neonRgb = () => document.documentElement.style.getPropertyValue("--neon-rgb") || "0, 162, 255";

function playEvolution(fromRank, rpg) {
  const box = $("#evolution");
  const text = `Rank ${fromRank} → ${rpg.rank}`;
  const t = $("#evo-text");
  t.textContent = text;
  t.dataset.text = text;
  $("#evo-sub").textContent = `${rpg.rank_title} · Level ${rpg.level}`;
  box.hidden = false;
  box.classList.remove("run");
  void box.offsetWidth;
  box.classList.add("run");
  Sfx.play("evolve");
  const [cx, cy] = [window.innerWidth / 2, window.innerHeight / 2];
  setTimeout(() => {
    applyTheme(rpg.color);
    FX.burst(cx, cy, neonRgb(), 3);
  }, 1100);
  setTimeout(() => { box.hidden = true; box.classList.remove("run"); }, 3700);
}

function onRpg(rpg) {
  if (prev.rank === null) {
    applyTheme(rpg.color);
  } else if (rpg.rank !== prev.rank) {
    if (document.hidden) pendingEvolution = { from: prev.rank, rpg };
    else playEvolution(prev.rank, rpg);
  } else if (!pendingEvolution) {
    applyTheme(rpg.color);
  }
  if (prev.level !== null && rpg.level > prev.level && rpg.rank === prev.rank) {
    toast(`Level up! You are now level ${rpg.level}.`);
    Sfx.play("levelup");
  }
  if (prev.exp !== null && rpg.total_exp > prev.exp) {
    toast(`Quest complete. +${rpg.total_exp - prev.exp} EXP`);
  }
  prev = { rank: rpg.rank, level: rpg.level, exp: rpg.total_exp };
}

function playPendingEvolution() {
  if (pendingEvolution && !document.hidden) {
    const { from, rpg } = pendingEvolution;
    pendingEvolution = null;
    playEvolution(from, rpg);
  }
}
document.addEventListener("visibilitychange", playPendingEvolution);
window.addEventListener("focus", playPendingEvolution);

/* ---------- tabs ---------- */

document.querySelectorAll(".tabs button").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".tabs button").forEach((b) => b.setAttribute("aria-selected", String(b === btn)));
    document.querySelectorAll(".tab").forEach((t) => (t.hidden = t.id !== `tab-${btn.dataset.tab}`));
  });
});

/* ---------- live numbers (gauges, clock, countdown) ---------- */

const pendingItems = () => status.items.filter((i) => !i.rest && !i.done);

// Display only. Rust decides when the Gate seals (plan.rs); this just shows it.
function live() {
  const pending = pendingItems().length;
  const done = status.done_today;
  const now = nowSecs();
  const d = status.decision;
  let timePct = 100, remaining = null;
  if (d && d.until) {
    const span = d.until - d.since;
    timePct = span > 0 ? Math.min(100, Math.max(0, ((now - d.since) / span) * 100)) : 100;
    remaining = d.until - now;
  }
  return { now, pending, done, d, timePct, remaining, tasksPct: pending + done ? (done / (pending + done)) * 100 : 100 };
}

function renderLive(withTimeline = true) {
  if (!status) return;
  const l = live();
  const d = l.d;

  $("#gv-quests").textContent = `${l.done}/${l.done + l.pending}`;
  setRing("#gf-quests", l.tasksPct);
  $("#gn-quests").textContent = l.pending === 0
    ? "Nothing left to do right now."
    : `${l.pending} quest${l.pending === 1 ? "" : "s"} left today.`;

  // right gauge: how long until the Gate next changes
  setRing("#gf-time", l.timePct);
  if (!d) {
    $("#gv-time").textContent = "—";
    $("#gl-time").textContent = "Next change";
    $("#gn-time").textContent = "";
  } else if (d.until) {
    $("#gv-time").textContent = l.remaining > 0 ? hms(l.remaining) : "Now";
    $("#gl-time").textContent = d.sealed
      ? (d.kind === "missed" ? "Auto-shift in" : d.kind === "chain" ? "Until break" : "Until clear")
      : "Gate closes in";
    $("#gn-time").textContent = d.sealed
      ? (d.kind === "missed" ? `Then it moves to your next holiday.` : `Gate can open at ${fmtTime(d.until)}.`)
      : `Next lock at ${fmtTime(d.until)}.`;
  } else {
    $("#gv-time").textContent = d.sealed ? "—" : "Free";
    $("#gl-time").textContent = d.sealed ? "Waiting for you" : "No lock ahead";
    $("#gn-time").textContent = d.sealed ? "Finish or reschedule to open the Gate." : "Nothing else is scheduled.";
  }

  const tag = $("#gate-tag");
  tag.textContent = d && d.sealed ? "Sealed" : "Open";
  tag.classList.toggle("ok", !(d && d.sealed));
  $("#why").textContent = d ? d.reason + "." : "";

  // what comes next
  const next = status.items.find((i) => !i.rest && !i.done && i.status === "upcoming");
  const lockFrom = config ? fmtMin(hhmmToMin(config.unplanned_lock_from)) : "6:00 PM";
  const unplanned = status.items.filter((i) => !i.rest && !i.done && i.status === "floating").length;
  $("#next-up").textContent = next
    ? `Next: ${next.title} at ${fmtTime(next.start)}`
    : unplanned && d && !d.sealed
      ? `${unplanned} unplanned task${unplanned === 1 ? "" : "s"}: they lock the Gate after ${lockFrom}.`
      : "";

  if (withTimeline) {
    const note = $("#schedule-note");
    note.hidden = !status.calendar_note;
    note.textContent = status.calendar_note || "";
    renderTimeline(l.now);
    const holiday = status.next_holiday ? `Next holiday: ${fmtDate(status.next_holiday)}. ` : "No holiday found in your calendar yet. ";
    $("#schedule-sum").textContent =
      `${holiday}Unplanned tasks lock the Gate after ${lockFrom}.`;
  }
}

let ticks = 0;
function clockTick() {
  const d = new Date();
  $("#clock").textContent = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit", hour12: true });
  if (status && !document.hidden) {
    renderLive(ticks % 15 === 0);
    renderTimer();
  }
  ticks++;
}
setInterval(clockTick, 1000);

/* ---------- status window (level, radar, bars) ---------- */

const ICONS = { vitals: "✚", intellect: "✦", perception: "◉", strength: "▲" };
const barNodes = new Map();
let radarRadii = [0.08, 0.08, 0.08, 0.08];

function ensureBars(bars) {
  if (barNodes.size) return;
  const ul = $("#bars");
  for (const b of bars) {
    const li = el("li", "stat");
    li.append(el("span", "stat-icon", ICONS[b.key] || "◆"));
    const main = el("div", "stat-main");
    const head = el("div", "stat-head");
    const name = el("span", "");
    const nums = el("span", "");
    head.title = b.name;
    head.append(el("b", "", b.short), name, nums);
    const track = el("div", "track");
    const fill = el("div", "fill");
    track.append(fill);
    main.append(head, track);
    li.append(main);
    ul.append(li);
    barNodes.set(b.key, { name, nums, fill });
  }
}

function drawRadar(radii) {
  const c = 90, R = 64;
  const [t, r, b, l] = radii.map((v) => R * v);
  $("#radar-poly").setAttribute("points", `${c},${c - t} ${c + r},${c} ${c},${c + b} ${c - l},${c}`);
}

function renderRpg() {
  const r = status.rpg;
  animateNumber($("#level"), r.level);
  $("#rank").textContent = `${r.rank}-Rank`;
  $("#rank-letter").textContent = r.rank;
  $("#rank-title").textContent = r.rank_title;
  $("#exp-text").textContent = `${r.into} / ${r.need}`;
  setRing("#gf-exp", r.need ? (r.into / r.need) * 100 : 0);

  ensureBars(r.bars);
  for (const b of r.bars) {
    const n = barNodes.get(b.key);
    n.name.textContent = `Lv ${b.level}`;
    n.nums.textContent = `${b.into}/${b.need}`;
    n.fill.style.width = `${b.need ? (b.into / b.need) * 100 : 0}%`;
  }
  const raw = r.bars.map((b) => b.level - 1 + (b.need ? b.into / b.need : 0));
  const top = Math.max(4, Math.max(...raw) * 1.2);
  const target = raw.map((v) => 0.08 + 0.92 * (v / top));
  const from = radarRadii;
  radarRadii = target;
  FX.tween(0, 1, 800, (k) => drawRadar(from.map((f, i) => f + (target[i] - f) * k)));
}

function renderStatus() {
  if (!status) return;
  const d = status.decision;
  const locked = status.locked;
  document.body.dataset.locked = String(locked);
  FX.setLocked(locked);
  $("#os-tag").textContent = status.os;
  $("#signin").hidden = status.authorized;
  const chip = $("#account");
  chip.hidden = !status.account;
  chip.textContent = status.account || "";

  renderRpg();

  $("#gate-state").textContent = locked ? "Penalty zone active" : "Gate open";
  let sub = d ? d.reason + "." : "";
  if (!status.authorized) sub = "Sign in with Google. The Gate seals itself whenever a quest is due.";
  else if (status.suppressed) sub = "Emergency release is in effect. Auto-enforcement resumes at midnight.";
  else if (locked && d && !d.sealed) sub = "Opening the Gate…";
  else if (!locked && d && d.sealed) sub = `${sub} Sealing…`;
  $("#gate-sub").textContent = sub;

  const err = $("#status-error");
  err.hidden = !status.error;
  err.textContent = status.error || "";
  $("#last-check").textContent = status.last_checked
    ? `Last checked ${new Date(status.last_checked * 1000).toLocaleTimeString([], TIME_OPTS)}`
    : "";
  $("#btn-refresh").disabled = busy || !status.authorized;

  renderQuests();
  renderMoved();
  renderLive();
  renderTimer();
  renderPenalty();
  renderSettingsState();
}

/* ---------- penalty zone ---------- */

function renderPenalty() {
  const locked = !!(status && status.locked);
  const c = config || { blocked_domains: [], blocked_apps: [] };
  $("#penalty-chip").textContent = locked ? "Sealed" : status && status.suppressed ? "Suspended" : "Standby";
  $("#penalty-copy").textContent = locked
    ? "Everything below is blocked right now. Finish what is due, or reschedule it, to open the Gate."
    : "These are sealed whenever a quest is due. In free time and breaks they stay open.";

  const ul = $("#penalty-tiles");
  ul.replaceChildren();
  const items = [
    ...c.blocked_domains.map((n) => ({ name: n, kind: "Site" })),
    ...c.blocked_apps.map((n) => ({ name: n, kind: "App" })),
  ];
  if (!items.length) {
    ul.append(el("li", "none", "Nothing configured yet. Add sites or apps in System settings."));
    return;
  }
  items.forEach((it, i) => {
    const li = el("li", "tile");
    li.style.setProperty("--d", `${((i * 0.83) % 5).toFixed(2)}s`);
    const lk = el("i", "lk");
    lk.setAttribute("aria-hidden", "true");
    li.append(lk, el("b", "", it.name), el("small", "", `${it.kind} · ${locked ? "access denied" : "standby"}`));
    ul.append(li);
  });
}

/* ---------- quests ---------- */

const GROUPS = [
  ["missed", "Missed: finish or reschedule"],
  ["active", "On now"],
  ["upcoming", "Up next"],
  ["floating", "Unplanned"],
];

function statusPills(q) {
  const pills = [];
  if (q.status === "missed") pills.push(["pill late", "MISSED"]);
  if (q.in_progress) pills.push(["pill prog", q.focus_secs ? `IN PROGRESS · ${fmtFocus(q.focus_secs)}` : "IN PROGRESS"]);
  else if (q.status === "active" || q.status === "upcoming" || q.status === "missed") pills.push(["pill dimmed", "NOT STARTED"]);
  if (q.status === "floating") pills.push(["pill dimmed", "UNPLANNED"]);
  if (q.overdue) pills.push(["pill late", "OVERDUE"]);
  if (q.moved) pills.push(["pill dimmed", "RESCHEDULED"]);
  return pills;
}

function questRow(q, ghost) {
  const working = completing.has(q.key);
  const classes = [`st-${q.status}`];
  if (q.overdue) classes.push("overdue");
  if (ghost) classes.push("done");
  if (!ghost && !knownQuests.has(q.key)) { classes.push("enter"); knownQuests.add(q.key); }
  const li = el("li", classes.join(" "));

  const btn = el("button", `hexbtn${working ? " working" : ""}`);
  btn.type = "button";
  btn.disabled = working || ghost;
  btn.setAttribute("aria-label", `Mark complete: ${q.title}`);
  btn.title = q.kind === "task" ? "Mark complete (also ticks it in Google Tasks)" : "Mark complete";
  btn.append(el("i", "", "✓"));
  btn.addEventListener("click", () => completeQuest(q, btn));

  const main = el("div", "q-main");
  main.append(el("span", "q-title", q.title));
  const meta = el("div", "q-meta");
  if (q.start && q.end) meta.append(el("span", "pill", `${fmtTime(q.start)} – ${fmtTime(q.end)}`));
  for (const [cls, text] of statusPills(q)) meta.append(el(...[ "span", cls, text ]));
  for (const tag of (q.reward && q.reward.tags) || []) meta.append(el("span", "pill tag", tag));
  if (meta.childElementCount) main.append(meta);

  const acts = el("div", "row-actions");
  const play = el("button", "actbtn", "▶");
  play.type = "button";
  play.disabled = ghost;
  play.title = "Start the focus timer on this quest";
  play.setAttribute("aria-label", `Start focus timer on ${q.title}`);
  play.addEventListener("click", () => startTimer(q.key));
  const move = el("button", "actbtn", "⟲");
  move.type = "button";
  move.disabled = ghost;
  move.title = q.status === "floating" ? "Give it a time slot or move it" : "Reschedule";
  move.setAttribute("aria-label", `Reschedule ${q.title}`);
  move.addEventListener("click", () => openReschedule(q));
  acts.append(play, move);

  const reward = el("div", "reward");
  reward.append(el("b", "", `+${q.reward ? q.reward.exp : 10}`), el("small", "", "EXP"));

  li.append(btn, main, acts, reward);
  return li;
}

function renderQuests() {
  const list = $("#quests");
  list.replaceChildren();
  $("#quest-intro").textContent =
    `[Daily Quest: ${new Date().toLocaleDateString([], { weekday: "short", day: "numeric", month: "short" })} has arrived.]`;

  const pending = pendingItems();
  let shown = 0;
  for (const [st, label] of GROUPS) {
    const rows = [
      ...[...ghosts.values()].filter((g) => g.status === st).map((g) => [g, true]),
      ...pending.filter((q) => q.status === st).map((q) => [q, false]),
    ];
    if (!rows.length) continue;
    const head = el("li", "group-title", label);
    head.setAttribute("role", "presentation");
    list.append(head);
    for (const [q, ghost] of rows) { list.append(questRow(q, ghost)); shown++; }
  }

  const badge = $("#quest-badge");
  badge.hidden = pending.length === 0;
  badge.textContent = String(pending.length);

  $("#quest-warning").hidden = pending.length === 0;
  const empty = $("#quests-empty");
  empty.hidden = shown > 0;
  $("#quests-empty-text").textContent = status.authorized
    ? (status.items.length ? "[Nothing due right now.]" : "[Nothing planned today.]")
    : "[Sign in with Google to receive your quests.]";
  empty.querySelector("svg").style.display = status.authorized ? "" : "none";
}

/** Ticks a quest. Tasks: Rust PATCHes the task in Google Tasks. Events: stored in the app. Both pay EXP. */
async function completeQuest(q, btn) {
  const rect = btn.getBoundingClientRect();
  const [x, y] = [rect.left + rect.width / 2, rect.top + rect.height / 2];
  completing.add(q.key);
  renderQuests();
  let ok = false;
  try {
    status = q.kind === "task"
      ? await call("complete_task", { taskId: q.id })
      : await call("complete_event", { key: q.key });
    ok = true;
  } catch (_) { /* toast shown; the quest stays in the list */ }
  completing.delete(q.key);

  if (ok) {
    ghosts.set(q.key, q);
    setTimeout(() => { ghosts.delete(q.key); if (status) renderQuests(); }, 950);
    FX.burst(x, y, neonRgb());
    FX.floatText(x, y, `+${q.reward ? q.reward.exp : 10} EXP`);
    Sfx.play("complete");
  }
  renderStatus();
  onRpg(status.rpg);
}

/* ---------- rescheduled list ---------- */

function renderMoved() {
  const box = $("#moved-box");
  const moved = status.moved || [];
  box.hidden = moved.length === 0;
  if (!moved.length) return;
  $("#moved-sum").textContent = `Rescheduled (${moved.length})`;
  const ul = $("#moved-list");
  ul.replaceChildren();
  for (const m of moved) {
    const li = el("li");
    const when = `${fmtDate(m.date)}${m.start_min != null ? " · " + fmtMin(m.start_min) : ""}`;
    const text = el("span", "moved-text");
    text.append(el("b", "", m.title), el("small", "", `${when}${m.auto ? " · moved by the System" : ""}`));
    const back = el("button", "btn ghost small", "Bring to today");
    back.type = "button";
    back.addEventListener("click", async () => {
      try {
        status = await call("reschedule", { key: m.key, mode: "date", date: todayStr(), startMin: null, durMin: null });
        renderStatus();
        toast(`${m.title} is back on today's list.`);
      } catch (_) {}
    });
    li.append(text, back);
    ul.append(li);
  }
}

/* ---------- reschedule dialog ---------- */

let rsTarget = null;

function openReschedule(q) {
  rsTarget = q;
  $("#rs-quest").textContent = `[${q.title}]`;
  $("#rs-error").hidden = true;
  const date = $("#rs-date");
  date.min = todayStr();
  date.value = q.status === "floating" ? todayStr() : q.status === "missed" ? todayStr() : tomorrowStr();
  // a sensible default start: the next half hour
  const n = new Date();
  const next = (Math.floor((n.getHours() * 60 + n.getMinutes()) / 30) + 1) * 30;
  setTime12($("#rs-time"), q.status === "floating" || q.status === "missed" ? next % 1440 : null);
  $("#rs-dur").value = String(q.dur_min || 30);
  const h = $("#rs-holiday");
  h.disabled = !status.next_holiday;
  h.textContent = status.next_holiday ? `Next holiday · ${fmtDate(status.next_holiday)}` : "Next holiday (none found)";
  $("#resched").hidden = false;
  $("#rs-date").focus();
}

function closeReschedule() {
  $("#resched").hidden = true;
  rsTarget = null;
}

async function submitReschedule(args) {
  if (!rsTarget) return;
  const q = rsTarget;
  try {
    status = await invoke("reschedule", { key: q.key, ...args });
    closeReschedule();
    Sfx.play("move");
    toast(`${q.title} was rescheduled.`);
    renderStatus();
  } catch (err) {
    const e = $("#rs-error");
    e.hidden = false;
    e.textContent = String(err);
    Sfx.play("error");
  }
}

$("#rs-holiday").addEventListener("click", () => submitReschedule({ mode: "holiday", date: null, startMin: null, durMin: Number($("#rs-dur").value) || null }));
$("#rs-tomorrow").addEventListener("click", () => {
  // same clock time tomorrow if it was a timed block, otherwise just "tomorrow"
  const start = rsTarget && rsTarget.start ? new Date(rsTarget.start * 1000) : null;
  submitReschedule({
    mode: "date", date: tomorrowStr(),
    startMin: start ? start.getHours() * 60 + start.getMinutes() : null,
    durMin: Number($("#rs-dur").value) || null,
  });
});
$("#rs-go").addEventListener("click", () => submitReschedule({
  mode: "date", date: $("#rs-date").value, startMin: getTime12($("#rs-time")), durMin: Number($("#rs-dur").value) || null,
}));
$("#rs-cancel").addEventListener("click", closeReschedule);
$("#resched").addEventListener("click", (e) => { if (e.target.id === "resched") closeReschedule(); });
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !$("#resched").hidden) closeReschedule(); });

/* ---------- focus timer (Pomodoro; the clock itself runs in Rust) ---------- */

const PHASE_LABEL = { idle: "Ready", focus: "Focus", short_break: "Short break", long_break: "Long break" };

function renderTimer() {
  if (!status) return;
  const t = status.timer;
  const running = t.phase !== "idle";
  const card = $("#timer-card");
  card.dataset.phase = t.phase;
  const total = Math.max(1, t.ends_at - t.started_at);
  const remaining = running ? Math.max(0, t.ends_at - nowSecs()) : 0;
  $("#timer-clock").textContent = running ? ms(remaining) : `${pad(config ? config.focus_min : 25)}:00`;
  $("#timer-phase").textContent = PHASE_LABEL[t.phase] || "Ready";
  setRing("#gf-timer", running ? ((total - remaining) / total) * 100 : 0);

  $("#timer-quest").textContent = !running
    ? "Press ▶ on a quest to work on it, or start a free session."
    : t.phase === "focus"
      ? (t.quest_title ? `Working on: ${t.quest_title}` : "Free focus session")
      : "Take a break. Stand up, stretch, drink some water.";
  const n = status.sessions_today;
  $("#timer-stats").textContent = `${n} session${n === 1 ? "" : "s"} today · ${fmtFocus(status.focus_secs_today)} of focus`;
  const b = $("#btn-timer");
  b.textContent = running ? (t.phase === "focus" ? "Stop" : "Skip break") : "Start focus";
}

async function startTimer(key) {
  try {
    status = await call("timer_start", { key });
    renderStatus();
    Sfx.play("ping");
  } catch (_) {}
}

$("#btn-timer").addEventListener("click", async () => {
  if (status.timer.phase === "idle") return startTimer(null);
  status = await invoke("timer_stop");
  renderStatus();
});

/* ---------- schedule ---------- */

function renderTimeline(now) {
  const ol = $("#timeline");
  ol.replaceChildren();
  const timed = status.items.filter((i) => i.start && i.end);
  for (const e of timed) {
    const state = e.rest ? "rest" : e.done ? "done" : e.status;
    const li = el("li", `${now >= e.end ? "past" : now >= e.start ? "active" : "future"} ${state}`);
    const rune = el("span", "rune");
    rune.setAttribute("aria-hidden", "true");
    const badge = {
      rest: "Break", done: "Done", missed: "Missed",
      active: e.in_progress ? "In progress" : "On now · not started",
      upcoming: e.in_progress ? "In progress" : "Upcoming",
    }[state] || "";
    li.append(rune, el("span", "when", `${fmtTime(e.start)} – ${fmtTime(e.end)}`), el("span", "title", e.title), el("span", "badge", badge));
    ol.append(li);
  }
  const empty = $("#schedule-empty");
  empty.hidden = timed.length > 0;
  empty.textContent = status.authorized
    ? "Nothing scheduled today. The Gate stays open."
    : "Sign in with Google to load your schedule.";
}

/* ---------- refresh ---------- */

$("#btn-refresh").addEventListener("click", async () => {
  setBusy(true);
  try {
    status = await call("refresh_status");
    onRpg(status.rpg);
  } catch (_) {}
  setBusy(false);
});

/* ---------- sign in ---------- */

async function signIn() {
  const buttons = document.querySelectorAll(".js-signin");
  const labels = [...buttons].map((b) => b.textContent);
  buttons.forEach((b) => { b.disabled = true; b.textContent = "Waiting for Google…"; });
  try {
    toast("Finish signing in with Google in your browser.");
    await call("google_login");
    status = await invoke("get_status");
    renderStatus();
    toast("Signed in. Loading your plan…");
    status = await call("refresh_status");
    renderStatus();
    onRpg(status.rpg);
  } catch (_) {
    /* toast already shown */
  } finally {
    buttons.forEach((b, i) => { b.disabled = false; b.textContent = labels[i]; });
  }
}
document.querySelectorAll(".js-signin").forEach((b) => b.addEventListener("click", signIn));

/* ---------- system settings ---------- */

function renderChips(ulId, items, onRemove, none) {
  const ul = $(ulId);
  ul.replaceChildren();
  if (!items.length) {
    ul.append(el("li", "none", none));
    return;
  }
  items.forEach((value, i) => {
    const li = el("li");
    const x = el("button", "", "✕");
    x.type = "button";
    x.setAttribute("aria-label", `Remove ${value}`);
    x.disabled = !!(status && status.locked);
    x.addEventListener("click", () => onRemove(i));
    li.append(el("span", "", value), x);
    ul.append(li);
  });
}

function renderSettings() {
  if (!config) return;
  renderChips("#domain-chips", config.blocked_domains, (i) => { config.blocked_domains.splice(i, 1); renderSettings(); renderPenalty(); }, "No sites yet");
  renderChips("#app-chips", config.blocked_apps, (i) => { config.blocked_apps.splice(i, 1); renderSettings(); renderPenalty(); }, "No apps yet");
  $("#in-list").value = config.task_list_id;
  $("#in-poll").value = config.poll_interval_secs;
  setTime12($("#in-lockfrom"), hhmmToMin(config.unplanned_lock_from));
  $("#in-grace").value = config.missed_grace_mins;
  $("#in-focus").value = config.focus_min;
  $("#in-short").value = config.short_break_min;
  $("#in-long").value = config.long_break_min;
  $("#in-every").value = config.long_break_every;
  $("#in-notif").checked = !!config.notifications;
  $("#in-sound").checked = Sfx.enabled;
  $("#in-vol").value = String(Math.round(Sfx.volume * 100));
  $("#in-cid").value = config.google_client_id;
  $("#in-secret").value = config.google_client_secret;
  renderSettingsState();
}

function renderSettingsState() {
  const locked = !!(status && status.locked);
  $("#settings-lock").hidden = !locked;
  document.querySelectorAll("#tab-settings input, #add-domain, #add-app, #btn-save").forEach((e) => {
    e.disabled = locked && !e.closest(".always"); // sound controls are local to this device and always work
  });
  $("#btn-logout").disabled = locked || !(status && status.authorized);
  $("#in-surrender").disabled = false;
  $("#btn-surrender").disabled = !locked;
  $("#auth-state").textContent = status && status.authorized
    ? `Signed in${status.account ? ` as ${status.account}` : ""}. Google Tasks: read and write. Calendar: read-only.`
    : "Not signed in yet.";
}

function addTo(inputId, key) {
  const input = $(inputId);
  const value = input.value.trim();
  if (!value) return;
  if (!config[key].includes(value)) config[key].push(value);
  input.value = "";
  renderSettings();
  renderPenalty();
}

$("#add-domain").addEventListener("click", () => addTo("#in-domain", "blocked_domains"));
$("#add-app").addEventListener("click", () => addTo("#in-app", "blocked_apps"));
$("#in-domain").addEventListener("keydown", (e) => e.key === "Enter" && addTo("#in-domain", "blocked_domains"));
$("#in-app").addEventListener("keydown", (e) => e.key === "Enter" && addTo("#in-app", "blocked_apps"));

// sound settings apply immediately (they live on this device, not in the Rust config)
$("#in-sound").addEventListener("change", (e) => { Sfx.setEnabled(e.target.checked); if (e.target.checked) Sfx.play("ping"); });
$("#in-vol").addEventListener("input", (e) => Sfx.setVolume(Number(e.target.value) / 100));
$("#in-vol").addEventListener("change", () => Sfx.play("complete"));
$("#btn-test-sound").addEventListener("click", () => Sfx.play("complete"));

function readForm() {
  const num = (id, fallback) => Number($(id).value) || fallback;
  config.task_list_id = $("#in-list").value;
  config.poll_interval_secs = num("#in-poll", 60);
  config.unplanned_lock_from = minToHhmm(getTime12($("#in-lockfrom")) ?? 18 * 60);
  config.missed_grace_mins = num("#in-grace", 30);
  config.focus_min = num("#in-focus", 25);
  config.short_break_min = num("#in-short", 5);
  config.long_break_min = num("#in-long", 15);
  config.long_break_every = num("#in-every", 4);
  config.notifications = $("#in-notif").checked;
  config.google_client_id = $("#in-cid").value;
  config.google_client_secret = $("#in-secret").value;
}

async function save() {
  readForm();
  config = await call("save_config", { config });
  renderSettings();
  renderPenalty();
  renderStatus();
}

$("#btn-save").addEventListener("click", async () => {
  try { await save(); toast("Settings saved."); } catch (_) {}
});

$("#btn-logout").addEventListener("click", async () => {
  try {
    await call("google_logout");
    status = await invoke("get_status");
    renderStatus();
    toast("Signed out of Google.");
  } catch (_) {}
});

$("#btn-surrender").addEventListener("click", async () => {
  setBusy(true);
  toast("Approve the administrator prompt to release the Gate.");
  try {
    await call("emergency_release", { phrase: $("#in-surrender").value });
    $("#in-surrender").value = "";
    toast("The Gate is open until midnight.");
  } catch (_) {}
  status = await invoke("get_status");
  setBusy(false);
});

/* ---------- gate state transitions ---------- */

function gateCentre() {
  const r = $("#gate-svg").getBoundingClientRect();
  return [r.left + r.width / 2, r.top + r.height / 2];
}

function onLockChange(nowLocked) {
  const [x, y] = gateCentre();
  const why = status.decision ? status.decision.reason : "";
  if (nowLocked) {
    toast(`Penalty zone active. ${why}.`);
    FX.shake($("#shell"));
    FX.burst(x, y, "255, 59, 85", 1.6);
    Sfx.play("seal");
  } else {
    toast(`The Gate is open. ${why}.`);
    const hero = $("#hero");
    hero.classList.remove("flash");
    void hero.offsetWidth;
    hero.classList.add("flash");
    FX.burst(x, y, neonRgb(), 2.2);
    Sfx.play("open");
  }
}

/** Sounds and toasts for changes that arrive from the background (a block was missed, the timer rang). */
function noteTransitions(next) {
  const missed = new Set(next.items.filter((i) => i.status === "missed" && !i.rest && !i.done).map((i) => i.key));
  if (prevMissed) {
    for (const k of missed) {
      if (!prevMissed.has(k)) {
        const q = next.items.find((i) => i.key === k);
        toast(`${q.title} was not finished. Reschedule it or finish it.`, "bad", 9000);
        Sfx.play("warn");
      }
    }
  }
  prevMissed = missed;

  const phase = next.timer.phase;
  if (prevPhase !== null && phase !== prevPhase) {
    if (prevPhase === "focus" && (phase === "short_break" || phase === "long_break")) {
      toast("Focus session complete. Time for a break.");
      Sfx.play("bell");
    } else if ((prevPhase === "short_break" || prevPhase === "long_break") && phase === "idle") {
      toast("Break over. Ready for the next session?");
      Sfx.play("ping");
    }
  }
  prevPhase = phase;
}

// The emblem tilts toward the pointer.
$("#hero").addEventListener("pointermove", (e) => {
  if (reducedMotion()) return;
  const r = $("#gate-stage").getBoundingClientRect();
  const dx = (e.clientX - (r.left + r.width / 2)) / window.innerWidth;
  const dy = (e.clientY - (r.top + r.height / 2)) / window.innerHeight;
  const svg = $("#gate-svg").style;
  svg.setProperty("--ry", `${(dx * 22).toFixed(2)}deg`);
  svg.setProperty("--rx", `${(-dy * 22).toFixed(2)}deg`);
});
$("#hero").addEventListener("pointerleave", () => {
  const svg = $("#gate-svg").style;
  svg.setProperty("--ry", "0deg");
  svg.setProperty("--rx", "0deg");
});

/* ---------- boot sequence ---------- */

async function bootSequence() {
  if (reducedMotion()) return;
  const box = $("#boot"), pre = $("#boot-text");
  const lines = [
    "[SYSTEM] Initializing…",
    "[SYSTEM] Linking quest log… OK",
    `[SYSTEM] Gate status: ${status && status.locked ? "SEALED" : "OPEN"}`,
    "[SYSTEM] Welcome back, Player.",
  ];
  let skipped = false;
  box.hidden = false;
  box.classList.remove("out");
  box.addEventListener("click", () => (skipped = true), { once: true });
  for (const line of lines) {
    for (const ch of line + "\n") {
      if (skipped) break;
      pre.textContent += ch;
      await sleep(ch === "\n" ? 140 : 16);
    }
  }
  if (!skipped) await sleep(250);
  box.classList.add("out");
  await sleep(650);
  box.hidden = true;
  pre.textContent = "";
}

function bootWhenVisible() {
  if (!document.hidden) { bootSequence(); return; }
  const once = () => {
    if (document.hidden) return;
    document.removeEventListener("visibilitychange", once);
    bootSequence();
  };
  document.addEventListener("visibilitychange", once);
}

/* ---------- boot ---------- */

async function boot() {
  initTime12($("#rs-time"), true);
  initTime12($("#in-lockfrom"), false);
  config = await invoke("get_config");
  status = await invoke("get_status");
  renderSettings();
  renderStatus();
  onRpg(status.rpg);
  noteTransitions(status);
  clockTick();
  bootWhenVisible();

  // Rust pushes a snapshot after every poll and whenever anything changes.
  await listen("gate-status", (event) => {
    const wasLocked = status.locked;
    status = event.payload;
    if (wasLocked !== status.locked) onLockChange(status.locked);
    noteTransitions(status);
    renderStatus();
    onRpg(status.rpg);
  });
}

boot().catch((e) => toast(`Startup failed: ${e}`, "bad", 12000));
