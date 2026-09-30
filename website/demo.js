// The looping demo: copy on the Mac, push off the edge (it ripples where it crosses), paste on
// the other computer, its sound plays through the Mac's headphones, then push back home.
// Positions are measured from the scene's layout each loop, so it works at any size, and
// ignore the tilt the scene settles out of as it scrolls in.
(() => {
  const $ = (id) => document.getElementById(id);
  const demo = $("demo");
  const cursor = $("cursor");
  const caption = $("caption");
  const keycap = $("keycap");
  const term = $("term");
  const termText = $("term-text");
  const noteText = $("note-text");
  const player = $("player");
  const phones = $("phones");
  const notes = $("notes");
  const steps = [...document.querySelectorAll(".step")];
  const macScreen = demo.querySelector(".mac-screen");
  const imacScreen = demo.querySelector(".imac-screen");
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (new URLSearchParams(location.search).has("video")) document.body.classList.add("video-mode");
  if (new URLSearchParams(location.search).has("og")) document.body.classList.add("og-mode");

  // Clicking a step restarts the demo from there: everything the running one is waiting on
  // (sleeps and moves) is given the token it started with, and gives up once that's stale.
  const STOP = Symbol("stop");
  let token = {};
  const sleep = (ms) => {
    const t = token;
    return new Promise((done, stop) => setTimeout(() => (t === token ? done() : stop(STOP)), ms));
  };

  // An element's box in the scene's own (untransformed) layout, in pixels.
  function layout(el) {
    let x = 0, y = 0;
    for (let n = el; n && n !== demo; n = n.offsetParent) {
      x += n.offsetLeft + (n === el ? 0 : n.clientLeft);
      y += n.offsetTop + (n === el ? 0 : n.clientTop);
    }
    return { x, y, w: el.offsetWidth, h: el.offsetHeight };
  }

  // A point inside an element, as fractions of the scene (0..1).
  function at(el, fx, fy) {
    const r = layout(el);
    return { x: (r.x + r.w * fx) / demo.clientWidth, y: (r.y + r.h * fy) / demo.clientHeight };
  }

  // The cursor moves by `translate` rather than left/top, so moving it doesn't lay out the scene.
  let pos = { x: 0.75, y: 0.5 };
  let size = { w: demo.clientWidth, h: demo.clientHeight };
  function place(p) {
    pos = p;
    cursor.style.translate = `${p.x * size.w}px ${p.y * size.h}px`;
  }
  new ResizeObserver(() => {
    size = { w: demo.clientWidth, h: demo.clientHeight };
    place(pos);
  }).observe(demo);

  const ease = (t) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2);
  function move(to, ms) {
    const from = pos;
    const run = token;
    return new Promise((done, stop) => {
      const start = performance.now();
      const step = (now) => {
        if (run !== token) return stop(STOP);
        const t = Math.min(1, (now - start) / ms);
        const k = ease(t);
        place({ x: from.x + (to.x - from.x) * k, y: from.y + (to.y - from.y) * k });
        t < 1 ? requestAnimationFrame(step) : done();
      };
      requestAnimationFrame(step);
    });
  }

  function say(step, text) {
    steps.forEach((s, i) => {
      s.classList.toggle("on", i === step);
      s.classList.toggle("done", i < step);
    });
    caption.style.opacity = 0;
    setTimeout(() => { caption.textContent = text; caption.style.opacity = 1; }, 250);
  }

  function key(text, near) {
    keycap.textContent = text;
    keycap.style.left = `${near.x * 100}%`;
    keycap.style.top = `${near.y * 100}%`;
    keycap.classList.add("show");
    return sleep(700).then(() => keycap.classList.remove("show"));
  }

  function click() {
    cursor.classList.remove("click");
    void cursor.offsetWidth;
    cursor.classList.add("click");
    return sleep(300);
  }

  async function type(el, text, ms = 55) {
    for (const ch of text) {
      el.textContent += ch;
      await sleep(ms);
    }
  }

  // The ripple where the cursor crosses, drawn the way the app draws it: this shader mirrors
  // `ripple::height` and `ripple::shade` in crates/core/src/ripple.rs, constants included.
  // The scene is small, so a point here is bigger than on a real screen.
  const POINTS_ACROSS_SCENE = 1150;
  const LIFETIME = 1.3;
  const MAX_RIPPLES = 8;
  const FRAGMENT = `
    precision highp float;
    uniform vec2 size;       // device pixels
    uniform float scale;     // device pixels per point
    uniform int count;
    uniform vec4 ripples[${MAX_RIPPLES}]; // origin in points (top-left, y down), age, strength
    float height(float d, float age, float strength) {
      float front = 190.0 * age;
      float behind = front - d;
      float width = 13.0 * (1.2 + 3.0 * age);
      float k = behind > 0.0 ? behind / width : behind / (13.0 * 0.35);
      float train = exp(-k * k);
      float fade = pow(1.0 - clamp(age / 1.3, 0.0, 1.0), 2.0);
      float spread = inversesqrt(1.0 + d / 30.0);
      return strength * fade * spread * train * sin(6.2831853 * behind / 13.0);
    }
    void main() {
      vec2 p = vec2(gl_FragCoord.x, size.y - gl_FragCoord.y) / scale;
      float h = 0.0;
      for (int i = 0; i < ${MAX_RIPPLES}; i++) {
        if (i >= count) break;
        h += height(length(p - ripples[i].xy), ripples[i].z, ripples[i].w);
      }
      float white = pow(clamp(h * 2.5, 0.0, 1.0), 1.5) * 0.45;
      float black = clamp(-h * 2.5, 0.0, 1.0) * 0.175;
      gl_FragColor = vec4(white, white, white, white + black * (1.0 - white)); // premultiplied
    }`;

  function rippleLayer(canvas) {
    const gl = canvas.getContext("webgl", { premultipliedAlpha: true, antialias: false });
    if (!gl) return { show() {} };
    const shader = (type, source) => {
      const s = gl.createShader(type);
      gl.shaderSource(s, source);
      gl.compileShader(s);
      return s;
    };
    const program = gl.createProgram();
    gl.attachShader(program, shader(gl.VERTEX_SHADER, "attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }"));
    gl.attachShader(program, shader(gl.FRAGMENT_SHADER, FRAGMENT));
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) return { show() {} };
    gl.useProgram(program);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW); // one triangle covering it
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    const u = (name) => gl.getUniformLocation(program, name);
    const uSize = u("size"), uScale = u("scale"), uCount = u("count"), uRipples = u("ripples");

    let live = [];
    let scale = 1;
    let running = false;
    function resize() {
      const dpr = Math.min(devicePixelRatio || 1, 2);
      canvas.width = Math.round(canvas.clientWidth * dpr);
      canvas.height = Math.round(canvas.clientHeight * dpr);
      scale = (demo.clientWidth * dpr) / POINTS_ACROSS_SCENE;
      gl.viewport(0, 0, canvas.width, canvas.height);
    }
    new ResizeObserver(resize).observe(canvas);
    resize();

    function frame(now) {
      live = live.filter((r) => (now - r.started) / 1000 < LIFETIME);
      const data = new Float32Array(MAX_RIPPLES * 4);
      live.forEach((r, i) => data.set([r.x * canvas.width / scale, r.y * canvas.height / scale, (now - r.started) / 1000, r.strength], i * 4));
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.uniform2f(uSize, canvas.width, canvas.height);
      gl.uniform1f(uScale, scale);
      gl.uniform1i(uCount, live.length);
      gl.uniform4fv(uRipples, data);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      running = live.length > 0;
      if (running) requestAnimationFrame(frame);
    }
    return {
      // A ripple at (fx, fy), fractions of this screen. Arriving is full strength, leaving half.
      show(fx, fy, strength) {
        live.push({ x: fx, y: fy, strength, started: performance.now() });
        if (live.length > MAX_RIPPLES) live.shift();
        if (!running) { running = true; requestAnimationFrame(frame); }
      },
    };
  }
  const ripples = { mac: rippleLayer($("ripple-mac")), imac: rippleLayer($("ripple-imac")) };

  // Crossing from one screen's edge to another's: a small ripple where it leaves, a full one
  // where it arrives, and the cursor carries on from the new edge.
  function cross(from, fromScreen, fromX, to, toScreen, toX) {
    const fy = (pos.y * demo.clientHeight - layout(fromScreen).y) / layout(fromScreen).h;
    ripples[from].show(fromX, fy, 0.5);
    const t = layout(toScreen);
    const y = Math.min(0.9, Math.max(0.1, (pos.y * demo.clientHeight - t.y) / t.h));
    ripples[to].show(toX, y, 1);
    place(at(toScreen, toX, y));
  }

  // Music notes flying from the other computer to the headphones.
  function sound() {
    const a = layout(player);
    const b = layout(phones);
    const x1 = a.x + a.w * 0.2, y1 = a.y + a.h * 0.35;
    const x2 = b.x + b.w * 0.3, y2 = b.y + b.h * 0.3;
    player.classList.add("playing");
    phones.classList.add("playing");
    ["♪", "♫", "♪", "♬", "♪"].forEach((glyph, i) => {
      setTimeout(() => {
        const n = document.createElement("span");
        n.textContent = glyph;
        const lift = demo.clientHeight * (0.25 + i * 0.04);
        n.style.offsetPath = `path("M ${x1} ${y1} Q ${(x1 + x2) / 2} ${Math.min(y1, y2) - lift} ${x2} ${y2}")`;
        notes.appendChild(n);
        setTimeout(() => n.remove(), 1900);
      }, i * 320);
    });
    return sleep(2600).then(() => {
      phones.classList.remove("playing");
      player.classList.remove("playing");
    });
  }

  const captions = [
    "Copy something on your Mac…",
    "…push the cursor off the edge…",
    "Your clipboard comes with you.",
    "So does its sound, through your Mac's headphones.",
    "Push back to come home.",
  ];
  const typed = "echo Kia ora from my Mac 👋";

  // How the scene looks as each step begins.
  function setup(step) {
    keycap.classList.remove("show");
    player.classList.remove("playing");
    phones.classList.remove("playing");
    notes.replaceChildren();
    noteText.classList.toggle("selected", step === 1);
    term.classList.toggle("focused", step === 2 || step === 3);
    termText.textContent = step >= 3 ? typed : "";
    if (step === 0) place(at(macScreen, 0.62, 0.8));
    else if (step === 1) place(at(noteText, 0.55, 0.6));
    else place(at(term, 0.55, 0.5));
  }

  const scenes = [
    async () => {
      await move(at(noteText, 0.55, 0.6), 900);
      await click();
      noteText.classList.add("selected");
      await key("⌘ C", at(noteText, 0.3, -1.6));
    },
    async () => {
      await move(at(macScreen, 0.0, 0.46), 1300);
      cross("mac", macScreen, 0, "imac", imacScreen, 1);
      say(1, "…and you're on the other computer. It ripples where you crossed.");
      await sleep(450);
      await move(at(term, 0.55, 0.5), 1300);
      await click();
      term.classList.add("focused");
      noteText.classList.remove("selected");
    },
    async () => {
      await key("⌘ V", at(term, 0.3, 0.12));
      termText.textContent = typed.slice(0, -3);
      await sleep(500);
      await type(termText, typed.slice(-3), 120);
      await sleep(600);
    },
    () => sound(),
    async () => {
      term.classList.remove("focused");
      await move(at(imacScreen, 1.0, 0.5), 1200);
      cross("imac", imacScreen, 1, "mac", macScreen, 0);
      await sleep(450);
      await move(at(macScreen, 0.55, 0.75), 1100);
      await sleep(1800);
    },
  ];

  // While the scene is scrolled away, the demo waits at the next step rather than playing to nobody.
  let showing = false;
  let waiting = [];
  new IntersectionObserver((entries) => {
    showing = entries.some((e) => e.isIntersecting);
    if (showing) waiting.splice(0).forEach((go) => go());
  }).observe(demo);
  const onScreen = () => (showing ? null : new Promise((go) => waiting.push(go)));

  async function loop(from = 0) {
    token = {};
    setup(from);
    try {
      for (let step = from; ; step = (step + 1) % scenes.length) {
        const run = token;
        await onScreen();
        if (run !== token) return;
        if (step === 0) setup(0);
        say(step, captions[step]);
        await scenes[step]();
      }
    } catch (e) {
      if (e !== STOP) throw e;
    }
  }

  steps.forEach((button, step) => button.addEventListener("click", () => {
    if (reduced) {
      setup(step);
      say(step, captions[step]);
    } else {
      loop(step);
    }
  }));

  if (reduced) {
    place(at(macScreen, 0.55, 0.75));
    steps.forEach((s) => s.classList.add("done"));
    caption.textContent = "Push the cursor off the edge of your Mac's screen and you're on the other computer.";
    termText.textContent = typed;
  } else {
    // Start once the scene is visible, so the first loop isn't missed.
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        io.disconnect();
        loop();
      }
    });
    io.observe(demo);
  }

  // Copy the Linux install command.
  document.querySelectorAll("[data-copy]").forEach((button) => {
    const text = button.querySelector(".cmd-text");
    // Kept from the start, so clicking again while the message shows can't lose it.
    const command = text.innerHTML;
    let timer;
    const say = (message, ok) => {
      clearTimeout(timer);
      text.textContent = message;
      button.classList.toggle("copied", ok);
      timer = setTimeout(() => { text.innerHTML = command; button.classList.remove("copied"); }, ok ? 1800 : 4000);
    };
    button.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(button.dataset.copy);
        say("Copied. Paste it into a terminal on your Linux computer.", true);
      } catch {
        say(button.dataset.copy, false);
        getSelection().selectAllChildren(text);
      }
    });
  });

  // Sections fade in as they scroll into view.
  const reveal = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (e.isIntersecting) {
        e.target.classList.add("in");
        reveal.unobserve(e.target);
      }
    }
  }, { rootMargin: "0px 0px -10% 0px" });
  document.querySelectorAll(".reveal").forEach((el) => reveal.observe(el));

  // Card animations only run while their card is on screen.
  const live = new IntersectionObserver((entries) => {
    for (const e of entries) e.target.classList.toggle("live", e.isIntersecting);
  });
  document.querySelectorAll(".art").forEach((el) => live.observe(el));
})();
