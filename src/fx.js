// System Gate visual effects. Pure decoration: nothing here touches app state.
//
//   FX.setColor("0, 162, 255")   neon colour (rank) as an "r, g, b" string
//   FX.setLocked(true)           sealed = red embers + faster rise, open = calm neon motes
//   FX.burst(x, y, "r, g, b")    spark explosion + shockwave ring at screen position
//   FX.floatText(x, y, "+20 EXP")  number that floats up and fades
//   FX.tween(from, to, ms, fn)   eased number animation (instant with reduced motion)
//   FX.shake(el)                 short screen shake
//
// The ambient canvas runs at ~30 fps, pauses while the window is hidden (this app
// lives in the tray, so it must cost nothing when nobody is looking) and draws a
// single still frame when the OS asks for reduced motion.

const FX = (() => {
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
  const WARN = "255, 59, 85";

  let neon = "0, 162, 255";
  let locked = false;

  /* ---------------- ambient background ---------------- */

  const bg = document.getElementById("bg");
  const bctx = bg.getContext("2d");
  let W = 0, H = 0, dpr = 1;
  let motes = [];
  let streaks = [];
  let gridOffset = 0;
  let last = 0;
  let running = false;

  const rand = (a, b) => a + Math.random() * (b - a);

  function makeMote(initial) {
    return {
      x: rand(0, W),
      y: initial ? rand(0, H) : H + 10,
      r: rand(0.6, 2.2),
      vy: rand(0.12, 0.5),
      vx: rand(-0.12, 0.12),
      phase: rand(0, Math.PI * 2),
      ember: Math.random() < 0.35, // red-orange embers while sealed
    };
  }

  function resize() {
    dpr = Math.min(window.devicePixelRatio || 1, 1.5);
    W = window.innerWidth;
    H = window.innerHeight;
    bg.width = Math.floor(W * dpr);
    bg.height = Math.floor(H * dpr);
    bg.style.width = `${W}px`;
    bg.style.height = `${H}px`;
    bctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const count = Math.max(28, Math.min(90, Math.round((W * H) / 16000)));
    motes = Array.from({ length: count }, () => makeMote(true));

    const burst = document.getElementById("burst");
    burst.width = Math.floor(W * dpr);
    burst.height = Math.floor(H * dpr);
    burst.style.width = `${W}px`;
    burst.style.height = `${H}px`;
    bursts.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    if (reduced.matches) drawBackground(0, 0);
  }

  function drawGrid(t) {
    // perspective floor: lines converge on a horizon in the lower third
    const horizon = H * 0.66;
    const vx = W / 2;
    const rgb = locked ? WARN : neon;
    bctx.lineWidth = 1;

    for (let i = 0; i < 14; i++) {
      const p = ((i + gridOffset) % 14) / 14;       // 0 (far) … 1 (near)
      const y = horizon + Math.pow(p, 2.2) * (H - horizon);
      bctx.strokeStyle = `rgba(${rgb}, ${0.05 + p * 0.12})`;
      bctx.beginPath();
      bctx.moveTo(0, y);
      bctx.lineTo(W, y);
      bctx.stroke();
    }
    for (let i = -10; i <= 10; i++) {
      bctx.strokeStyle = `rgba(${rgb}, ${0.05 + (1 - Math.abs(i) / 11) * 0.08})`;
      bctx.beginPath();
      bctx.moveTo(vx + i * 6, horizon);
      bctx.lineTo(vx + i * (W / 6), H);
      bctx.stroke();
    }
    const g = bctx.createLinearGradient(0, horizon - 40, 0, horizon + 30);
    g.addColorStop(0, `rgba(${rgb}, 0)`);
    g.addColorStop(0.5, `rgba(${rgb}, ${locked ? 0.1 : 0.07})`);
    g.addColorStop(1, `rgba(${rgb}, 0)`);
    bctx.fillStyle = g;
    bctx.fillRect(0, horizon - 40, W, 70);
  }

  function drawBackground(t, dt) {
    bctx.clearRect(0, 0, W, H);
    drawGrid(t);

    // rising motes
    const speed = locked ? 1.7 : 1;
    for (const m of motes) {
      m.y -= m.vy * speed * (dt / 16);
      m.x += m.vx * (dt / 16) + Math.sin(t / 1800 + m.phase) * 0.12;
      if (m.y < -10) Object.assign(m, makeMote(false));
      const tw = 0.5 + 0.5 * Math.sin(t / 700 + m.phase);
      const rgb = locked && m.ember ? "255, 140, 60" : locked ? WARN : neon;
      bctx.fillStyle = `rgba(${rgb}, ${0.18 + tw * 0.5})`;
      bctx.beginPath();
      bctx.arc(m.x, m.y, m.r, 0, Math.PI * 2);
      bctx.fill();
      if (m.r > 1.6) {
        bctx.fillStyle = `rgba(${rgb}, ${0.05 + tw * 0.07})`;
        bctx.beginPath();
        bctx.arc(m.x, m.y, m.r * 4, 0, Math.PI * 2);
        bctx.fill();
      }
    }

    // data streaks: thin lines that fall fast and fade
    if (!reduced.matches && Math.random() < 0.025 && streaks.length < 6) {
      streaks.push({ x: rand(0, W), y: rand(-80, H * 0.3), len: rand(50, 160), v: rand(5, 11), life: 1 });
    }
    streaks = streaks.filter((s) => s.life > 0 && s.y < H + 200);
    for (const s of streaks) {
      s.y += s.v * (dt / 16);
      s.life -= 0.012 * (dt / 16);
      const g = bctx.createLinearGradient(s.x, s.y - s.len, s.x, s.y);
      const rgb = locked ? WARN : neon;
      g.addColorStop(0, `rgba(${rgb}, 0)`);
      g.addColorStop(1, `rgba(${rgb}, ${0.5 * Math.max(0, s.life)})`);
      bctx.strokeStyle = g;
      bctx.lineWidth = 1.5;
      bctx.beginPath();
      bctx.moveTo(s.x, s.y - s.len);
      bctx.lineTo(s.x, s.y);
      bctx.stroke();
    }
  }

  function loop(ts) {
    if (!running) return;
    requestAnimationFrame(loop);
    if (ts - last < 33) return; // ~30 fps is plenty for ambience
    const dt = Math.min(64, ts - last || 33);
    last = ts;
    gridOffset += 0.012 * (dt / 16);
    drawBackground(ts, dt);
  }

  function start() {
    if (running || reduced.matches || document.hidden) return;
    running = true;
    last = 0;
    requestAnimationFrame(loop);
  }
  function stop() { running = false; }

  document.addEventListener("visibilitychange", () => (document.hidden ? stop() : start()));
  reduced.addEventListener?.("change", () => (reduced.matches ? (stop(), drawBackground(0, 0)) : start()));
  window.addEventListener("resize", resize);

  /* ---------------- bursts ---------------- */

  const bursts = {
    ctx: document.getElementById("burst").getContext("2d"),
    sparks: [],
    rings: [],
    on: false,
  };

  function burstLoop(ts) {
    const c = bursts.ctx;
    c.clearRect(0, 0, W, H);
    const dt = Math.min(48, ts - (burstLoop.t || ts));
    burstLoop.t = ts;
    const k = dt / 16;

    bursts.rings = bursts.rings.filter((r) => r.life > 0);
    for (const r of bursts.rings) {
      r.life -= 0.022 * k;
      r.r += (r.max - r.r) * 0.09 * k;
      c.strokeStyle = `rgba(${r.rgb}, ${Math.max(0, r.life) * 0.9})`;
      c.lineWidth = 2 + r.life * 3;
      c.beginPath();
      c.arc(r.x, r.y, r.r, 0, Math.PI * 2);
      c.stroke();
    }

    bursts.sparks = bursts.sparks.filter((s) => s.life > 0);
    for (const s of bursts.sparks) {
      s.life -= 0.018 * k;
      s.vy += 0.07 * k; // gravity
      s.x += s.vx * k;
      s.y += s.vy * k;
      c.fillStyle = `rgba(${s.rgb}, ${Math.max(0, s.life)})`;
      c.shadowColor = `rgba(${s.rgb}, 1)`;
      c.shadowBlur = 8;
      c.fillRect(s.x, s.y, s.size, s.size);
    }
    c.shadowBlur = 0;

    if (bursts.sparks.length || bursts.rings.length) {
      requestAnimationFrame(burstLoop);
    } else {
      bursts.on = false;
      c.clearRect(0, 0, W, H);
    }
  }

  function burst(x, y, rgb = neon, power = 1) {
    if (reduced.matches) return;
    const n = Math.round(30 * power);
    for (let i = 0; i < n; i++) {
      const a = rand(0, Math.PI * 2);
      const v = rand(1.5, 6.5) * power;
      bursts.sparks.push({
        x, y, vx: Math.cos(a) * v, vy: Math.sin(a) * v - 1.5,
        size: rand(1.5, 3.6), life: rand(0.7, 1.1),
        rgb: Math.random() < 0.25 ? "255, 255, 255" : rgb,
      });
    }
    bursts.rings.push({ x, y, r: 6, max: 70 * power, life: 1, rgb });
    if (!bursts.on) {
      bursts.on = true;
      burstLoop.t = 0;
      requestAnimationFrame(burstLoop);
    }
  }

  function floatText(x, y, text) {
    const el = document.createElement("div");
    el.className = "float-text";
    el.textContent = text; // textContent only
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
    document.body.appendChild(el);
    setTimeout(() => el.remove(), 1500);
  }

  /* ---------------- helpers ---------------- */

  const ease = (t) => 1 - Math.pow(1 - t, 3);

  /** Calls fn(value) every frame while easing from → to. Instant with reduced motion. */
  function tween(from, to, ms, fn) {
    if (reduced.matches || from === to || ms <= 0) { fn(to); return; }
    const t0 = performance.now();
    const step = (now) => {
      const t = Math.min(1, (now - t0) / ms);
      fn(from + (to - from) * ease(t));
      if (t < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }

  function shake(el) {
    if (reduced.matches || !el) return;
    el.classList.remove("shake");
    void el.offsetWidth; // restart the animation
    el.classList.add("shake");
    setTimeout(() => el.classList.remove("shake"), 600);
  }

  resize();
  start();

  return {
    setColor(rgb) { neon = rgb; if (reduced.matches) drawBackground(0, 0); },
    setLocked(on) { locked = on; if (reduced.matches) drawBackground(0, 0); },
    burst, floatText, tween, shake,
  };
})();
