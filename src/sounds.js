// Sound effects, synthesized with WebAudio. No audio files: every sound is a few
// oscillators and a noise burst, so nothing extra ships and nothing can fail to load.
//
//   Sfx.play("complete")   Sfx.setEnabled(false)   Sfx.setVolume(0.6)
//
// Sounds: tick, complete, levelup, evolve, seal, open, bell, ping, warn, error, move.
// The browser only lets audio start after a click or key press, so sounds triggered by
// background events before your first interaction stay silent. The choice (on/off,
// volume) is remembered in this app's local storage.

const Sfx = (() => {
  const KEY = "sg.sound";
  let prefs = { on: true, vol: 0.6 };
  try { prefs = { ...prefs, ...JSON.parse(localStorage.getItem(KEY) || "{}") }; } catch (_) { /* default */ }

  let ctx = null;
  let master = null;

  function save() {
    try { localStorage.setItem(KEY, JSON.stringify(prefs)); } catch (_) { /* private mode */ }
  }

  function ensure() {
    if (!ctx) {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (!AC) return null;
      ctx = new AC();
      master = ctx.createGain();
      master.gain.value = prefs.vol * 0.5; // headroom: the synth is bright
      master.connect(ctx.destination);
    }
    if (ctx.state === "suspended") ctx.resume().catch(() => {});
    return ctx;
  }

  // First interaction unlocks audio.
  for (const ev of ["pointerdown", "keydown"]) {
    window.addEventListener(ev, () => ensure(), { once: true, capture: true });
  }

  /** One enveloped oscillator note. `t` = seconds from now. */
  function tone({ f, f2 = null, t = 0, d = 0.2, type = "sine", g = 0.5, attack = 0.005 }) {
    const c = ctx, at = c.currentTime + t;
    const o = c.createOscillator(), env = c.createGain();
    o.type = type;
    o.frequency.setValueAtTime(f, at);
    if (f2) o.frequency.exponentialRampToValueAtTime(f2, at + d);
    env.gain.setValueAtTime(0.0001, at);
    env.gain.exponentialRampToValueAtTime(g, at + attack);
    env.gain.exponentialRampToValueAtTime(0.0001, at + d);
    o.connect(env).connect(master);
    o.start(at);
    o.stop(at + d + 0.05);
  }

  /** Filtered noise burst: clanks, whooshes. */
  function noise({ t = 0, d = 0.2, f = 1200, f2 = null, q = 1.2, g = 0.4, type = "bandpass" }) {
    const c = ctx, at = c.currentTime + t;
    const len = Math.max(1, Math.floor(c.sampleRate * d));
    const buf = c.createBuffer(1, len, c.sampleRate);
    const data = buf.getChannelData(0);
    for (let i = 0; i < len; i++) data[i] = Math.random() * 2 - 1;
    const src = c.createBufferSource();
    src.buffer = buf;
    const filt = c.createBiquadFilter();
    filt.type = type;
    filt.Q.value = q;
    filt.frequency.setValueAtTime(f, at);
    if (f2) filt.frequency.exponentialRampToValueAtTime(f2, at + d);
    const env = c.createGain();
    env.gain.setValueAtTime(g, at);
    env.gain.exponentialRampToValueAtTime(0.0001, at + d);
    src.connect(filt).connect(env).connect(master);
    src.start(at);
  }

  const N = { C4: 261.6, E4: 329.6, G4: 392, A4: 440, C5: 523.3, D5: 587.3, E5: 659.3, G5: 784, A5: 880, C6: 1046.5, E6: 1318.5 };

  const sounds = {
    // a light UI click
    tick: () => tone({ f: 1500, f2: 900, d: 0.04, type: "square", g: 0.12 }),

    // quest complete: bright rising arpeggio with a sparkle
    complete: () => {
      [N.C5, N.E5, N.G5, N.C6].forEach((f, i) => tone({ f, t: i * 0.07, d: 0.28, type: "triangle", g: 0.4 }));
      tone({ f: N.E6, t: 0.3, d: 0.5, g: 0.18 });
      noise({ t: 0.28, d: 0.35, f: 6000, f2: 9000, type: "highpass", g: 0.05, q: 0.5 });
    },

    // level up: a heroic fanfare
    levelup: () => {
      [[N.C5, 0], [N.E5, 0.1], [N.G5, 0.2], [N.C6, 0.32]].forEach(([f, t]) => tone({ f, t, d: 0.35, type: "sawtooth", g: 0.22 }));
      [N.E5, N.G5, N.C6].forEach((f) => tone({ f, t: 0.5, d: 0.9, type: "triangle", g: 0.28 }));
      tone({ f: N.C4, t: 0.5, d: 0.9, type: "sine", g: 0.3 });
    },

    // rank evolution: deep rise, glitchy sweep, big chord
    evolve: () => {
      tone({ f: 55, f2: 220, d: 1.4, type: "sawtooth", g: 0.3 });
      noise({ d: 1.4, f: 300, f2: 6000, q: 2, g: 0.35 });
      for (let i = 0; i < 6; i++) tone({ f: 400 + Math.random() * 1600, t: 0.3 + i * 0.12, d: 0.06, type: "square", g: 0.12 });
      [N.C4, N.E4, N.G4, N.C5, N.E5].forEach((f) => tone({ f, t: 1.4, d: 1.8, type: "triangle", g: 0.26 }));
      noise({ t: 1.4, d: 0.6, f: 8000, type: "highpass", g: 0.15, q: 0.5 });
    },

    // Gate seals: heavy thud, metallic clank, falling alarm
    seal: () => {
      tone({ f: 110, f2: 32, d: 0.5, type: "sine", g: 0.7 });
      noise({ d: 0.18, f: 2200, q: 6, g: 0.5 });
      noise({ t: 0.06, d: 0.35, f: 900, f2: 400, q: 3, g: 0.25 });
      tone({ f: 440, f2: 220, t: 0.18, d: 0.35, type: "square", g: 0.14 });
    },

    // Gate opens: rising whoosh and a clear chime
    open: () => {
      noise({ d: 0.7, f: 200, f2: 5000, q: 1.5, g: 0.28 });
      tone({ f: 220, f2: 880, d: 0.6, type: "sine", g: 0.3 });
      [N.G5, N.C6, N.E6].forEach((f, i) => tone({ f, t: 0.45 + i * 0.1, d: 0.7, type: "triangle", g: 0.26 }));
    },

    // Pomodoro bell: inharmonic partials with a long decay
    bell: () => {
      [[880, 0.5, 1.8], [1760, 0.25, 1.4], [2640, 0.15, 1.0], [3590, 0.08, 0.7]].forEach(([f, g, d]) => tone({ f, d, g, type: "sine", attack: 0.002 }));
      tone({ f: 880, t: 0.55, d: 1.6, g: 0.3 });
    },

    // notification-style two-note ping
    ping: () => {
      tone({ f: N.E5, d: 0.16, type: "sine", g: 0.35 });
      tone({ f: N.A5, t: 0.12, d: 0.3, type: "sine", g: 0.35 });
    },

    // something needs attention (missed block)
    warn: () => {
      [0, 0.22].forEach((t) => tone({ f: 520, f2: 330, t, d: 0.2, type: "square", g: 0.2 }));
    },

    // an error
    error: () => {
      tone({ f: 200, f2: 120, d: 0.25, type: "sawtooth", g: 0.3 });
      tone({ f: 150, f2: 90, t: 0.12, d: 0.25, type: "square", g: 0.18 });
    },

    // rescheduled
    move: () => {
      tone({ f: 600, f2: 900, d: 0.12, type: "triangle", g: 0.3 });
      tone({ f: 900, f2: 1200, t: 0.1, d: 0.14, type: "triangle", g: 0.25 });
    },
  };

  return {
    play(name) {
      if (!prefs.on || !sounds[name]) return;
      if (!ensure() || ctx.state !== "running") return;
      try { sounds[name](); } catch (_) { /* audio must never break the UI */ }
    },
    setEnabled(on) { prefs.on = !!on; save(); if (on) ensure(); },
    setVolume(v) {
      prefs.vol = Math.min(1, Math.max(0, v));
      save();
      if (master) master.gain.value = prefs.vol * 0.5;
    },
    get enabled() { return prefs.on; },
    get volume() { return prefs.vol; },
  };
})();
