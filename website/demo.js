// The looping demo: copy on the Mac, push off the edge, paste on the other computer, its sound
// plays through the Mac's headphones, then push back home. Positions are measured from the
// scene's elements each loop, so it works at any size.
(() => {
  const $ = (id) => document.getElementById(id);
  const demo = $("demo");
  const cursor = $("cursor");
  const caption = $("caption");
  const keycap = $("keycap");
  const term = $("term");
  const termText = $("term-text");
  const noteText = $("note-text");
  const phones = $("phones");
  const notes = $("notes");
  const macScreen = demo.querySelector(".mac-screen");
  const imacScreen = demo.querySelector(".imac-screen");
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (new URLSearchParams(location.search).has("video")) document.body.classList.add("video-mode");

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const box = () => demo.getBoundingClientRect();

  // A point inside an element, as fractions of the scene (0..1).
  function at(el, fx, fy) {
    const d = box();
    const r = el.getBoundingClientRect();
    return { x: (r.left - d.left + r.width * fx) / d.width, y: (r.top - d.top + r.height * fy) / d.height };
  }

  let pos = { x: 0.75, y: 0.5 };
  function place(p) {
    pos = p;
    cursor.style.left = `${p.x * 100}%`;
    cursor.style.top = `${p.y * 100}%`;
  }

  const ease = (t) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2);
  function move(to, ms) {
    const from = pos;
    return new Promise((done) => {
      const start = performance.now();
      const step = (now) => {
        const t = Math.min(1, (now - start) / ms);
        const k = ease(t);
        place({ x: from.x + (to.x - from.x) * k, y: from.y + (to.y - from.y) * k });
        t < 1 ? requestAnimationFrame(step) : done();
      };
      requestAnimationFrame(step);
    });
  }

  function say(text) {
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

  function glow(el, screen, side) {
    const d = box();
    const r = screen.getBoundingClientRect();
    el.style.left = `${((side === "left" ? r.left : r.right) - d.left) / d.width * 100 - 0.25}%`;
    el.style.top = `${(r.top - d.top) / d.height * 100 + 8}%`;
    el.style.height = `${r.height / d.height * 100 - 16}%`;
    el.classList.remove("on");
    void el.offsetWidth;
    el.classList.add("on");
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

  // Music notes flying from the other computer to the headphones.
  function sound() {
    const d = box();
    const a = imacScreen.getBoundingClientRect();
    const b = phones.getBoundingClientRect();
    const x1 = a.left - d.left + a.width * 0.7, y1 = a.top - d.top + a.height * 0.75;
    const x2 = b.left - d.left + b.width * 0.3, y2 = b.top - d.top + b.height * 0.3;
    phones.classList.add("playing");
    ["♪", "♫", "♪", "♬", "♪"].forEach((glyph, i) => {
      setTimeout(() => {
        const n = document.createElement("span");
        n.textContent = glyph;
        const lift = 60 + i * 12;
        n.style.offsetPath = `path("M ${x1} ${y1} Q ${(x1 + x2) / 2} ${Math.min(y1, y2) - lift} ${x2} ${y2}")`;
        notes.appendChild(n);
        setTimeout(() => n.remove(), 1900);
      }, i * 320);
    });
    return sleep(2600).then(() => phones.classList.remove("playing"));
  }

  async function loop() {
    for (;;) {
      termText.textContent = "";
      term.classList.remove("focused");
      noteText.classList.remove("selected");
      place(at(macScreen, 0.62, 0.8));
      say("Copy something on your Mac…");
      await move(at(noteText, 0.5, 0.6), 900);
      await click();
      noteText.classList.add("selected");
      await key("⌘ C", at(noteText, 0.35, -1.8));

      say("…push the cursor off the edge…");
      await move(at(macScreen, 0.0, 0.5), 1300);
      glow($("glow-mac"), macScreen, "left");
      glow($("glow-imac"), imacScreen, "right");
      place(at(imacScreen, 1.0, 0.5));
      say("…and you're on the other computer.");
      await move(at(term, 0.55, 0.62), 1100);
      await click();
      term.classList.add("focused");
      noteText.classList.remove("selected");

      say("Your clipboard comes with you.");
      await key("⌘ V", at(term, 0.3, -0.35));
      termText.textContent = "echo Kia ora from my Mac";
      await sleep(500);
      await type(termText, " 👋", 120);
      await sleep(600);

      say("So does its sound, through your Mac's headphones.");
      await sound();

      say("Push back to come home.");
      await move(at(imacScreen, 1.0, 0.45), 1200);
      glow($("glow-imac"), imacScreen, "right");
      glow($("glow-mac"), macScreen, "left");
      place(at(macScreen, 0.0, 0.45));
      await move(at(macScreen, 0.55, 0.75), 1000);
      await sleep(1600);
    }
  }

  if (reduced) {
    place(at(macScreen, 0.55, 0.75));
    caption.textContent = "Push the cursor off the edge of your Mac's screen and you're on the other computer.";
    termText.textContent = "echo Kia ora from my Mac 👋";
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
  const copy = $("copy");
  copy?.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText($("cmd").textContent);
      copy.textContent = "Copied";
      setTimeout(() => (copy.textContent = "Copy"), 1500);
    } catch {}
  });
})();
