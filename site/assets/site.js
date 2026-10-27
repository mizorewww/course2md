// Behaviour of the course2md landing page, ported from the design's component
// logic (LandingEn.dc.html / Landing.dc.html): platform detection, the
// scroll-driven story, the terminal demo, tabs and copy buttons. Strings come
// from the page's JSON data block so one script serves every language.
(function () {
  "use strict";
  var root = document.documentElement;
  root.classList.add("js");
  if (!(window.CSS && CSS.supports && CSS.supports("animation-timeline: view()")) && "IntersectionObserver" in window) {
    root.classList.add("io");
  }

  function clamp(v) { return Math.max(0, Math.min(1, v)); }
  function $(sel, ctx) { return (ctx || document).querySelector(sel); }
  function $$(sel, ctx) { return Array.prototype.slice.call((ctx || document).querySelectorAll(sel)); }
  function toggle(el, cls, on) { if (el) el.classList.toggle(cls, !!on); }

  function init() {
    if (!$("#c2m-data")) return;
    var data = {};
    try { data = JSON.parse($("#c2m-data").textContent); } catch (e) {}

    var reduce = false;
    try { reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches; } catch (e) {}

    // Platform ------------------------------------------------------------
    var detected = "other";
    try {
      var nav = window.navigator || {};
      var ua = nav.userAgent || "";
      var plat = (nav.userAgentData && nav.userAgentData.platform) || nav.platform || "";
      var mobile = /Android|iPhone|iPad|iPod|Mobile/i.test(ua) || (nav.userAgentData && nav.userAgentData.mobile);
      if (!mobile) {
        if (/Mac/i.test(plat) || /Macintosh/i.test(ua)) detected = "macos";
        else if (/Win/i.test(plat) || /Windows/i.test(ua)) detected = "windows";
        else if (/Linux|X11/i.test(plat + ua)) detected = "linux";
      }
    } catch (e) {}

    var cta = (data.ctas || {})[detected] || (data.ctas || {}).other;
    if (cta) {
      var ctaLink = $("#cta-main");
      ctaLink.setAttribute("href", cta.href);
      $("#cta-label").textContent = cta.label;
      $("#cta-note").textContent = cta.note;
    }

    // Segmented tabs (platform downloads, CLI install method)
    function tabs(group, onPick) {
      var buttons = $$("[role=tab]", group);
      function pick(i) {
        buttons.forEach(function (b, j) { b.setAttribute("aria-selected", i === j ? "true" : "false"); });
        onPick(i);
      }
      buttons.forEach(function (b, i) { b.addEventListener("click", function () { pick(i); }); });
      return pick;
    }
    var keys = ["macos", "windows", "linux"];
    var panels = keys.map(function (k) { return document.getElementById("panel-" + k); });
    var pickPlatform = tabs($("#platform-tabs"), function (i) {
      panels.forEach(function (p, j) { p.hidden = i !== j; });
    });
    pickPlatform(Math.max(0, keys.indexOf(detected)));
    var detectedNames = data.detected || {};
    if (detectedNames[detected]) {
      $("#detected-text").textContent = detectedNames[detected];
      $("#detected").hidden = false;
    }

    var cliCmds = data.cliCmds || [];
    var cliCode = $("#cli-cmd");
    tabs($("#cli-tabs"), function (i) { cliCode.textContent = cliCmds[i]; });

    // Copy buttons: "Copy" becomes "Copied" for 1.6 s; only one at a time.
    var copyTimer = null;
    var copyButtons = $$("button.copy");
    copyButtons.forEach(function (button) {
      button.addEventListener("click", function () {
        var code = button.parentNode.querySelector("code");
        try { navigator.clipboard.writeText(code.textContent); } catch (e) {}
        copyButtons.forEach(function (b) { b.textContent = data.copy; });
        button.textContent = data.copied;
        if (copyTimer) clearTimeout(copyTimer);
        copyTimer = setTimeout(function () { button.textContent = data.copy; }, 1600);
      });
    });

    // Story ---------------------------------------------------------------
    var story = $("#story");
    var pin = $(".story-pin", story);
    var N = 7, SN = 115, SG = 40;
    var steps = $$(".step", story);
    var scenes = $$(".scene", story);
    var bar = $(".story-bar span", story);
    var url = data.url || "";
    var typedEl = $("#typed");
    var previewBtn = $(".preview-btn", story);
    var startBtn = $(".start-btn", story);
    var shots = $$(".shot", story);
    var shotCount = $("#shot-count");
    var shotBar = $(".shots-bar span", story);
    var subs = $$(".sub-line", story);
    var ai = $(".sc-ai", story);
    var aiChip = $(".ai-chip", story);
    var aiSum = $(".ai-sum", story);
    var readBody = $(".reader-body", story);
    var toc = $$(".toc-item", story);
    var menu = $$(".menu-item", story);
    var files = $$(".file", story);
    var last = {};

    function progress() {
      var total = SN * SG;
      var pinH = pin.offsetHeight;
      var top = story.getBoundingClientRect().top;
      return { p: clamp(-top / (total - pinH)), total: total, pinH: pinH };
    }

    function renderStory() {
      var p = progress().p;
      var pos = Math.min(N - 0.0001, p * N);
      var scene = Math.floor(pos);
      var q = pos - scene;

      if (last.scene !== scene) {
        last.scene = scene;
        steps.forEach(function (st, i) {
          var on = scene === i;
          st.classList.toggle("story-step-on", on);
          st.classList.toggle("story-step-idle", !on);
          st.classList.toggle("is-past", scene > i);
          st.setAttribute("aria-current", on ? "step" : "false");
        });
        scenes.forEach(function (sc, i) {
          sc.classList.toggle("is-on", scene === i);
          sc.classList.toggle("is-past", scene > i);
          sc.setAttribute("aria-hidden", scene === i ? "false" : "true");
        });
      }
      bar.style.width = Math.round(p * 100) + "%";

      var typed = scene === 0 ? url.slice(0, Math.floor(clamp(q * 1.35) * url.length)) : url;
      if (typedEl.textContent !== typed) typedEl.textContent = typed;
      toggle(previewBtn, "is-done", scene === 0 && q > 0.78);
      toggle(startBtn, "is-hot", scene === 1 && q > 0.6);

      var shotQ = scene > 2 ? 1 : (scene === 2 ? clamp(q * 1.15) : 0);
      shots.forEach(function (s, i) { toggle(s, "is-on", shotQ * shots.length > i); });
      shotCount.textContent = String(Math.round(69 * shotQ));
      shotBar.style.width = Math.round(shotQ * 100) + "%";

      var subQ = scene > 3 ? 1 : (scene === 3 ? clamp(q * 1.2) : 0);
      subs.forEach(function (s, i) { toggle(s, "is-on", subQ * subs.length > i); });

      var aiQ = scene > 4 ? 1 : (scene === 4 ? q : 0);
      var cleaned = aiQ > 0.4;
      toggle(ai, "is-clean", cleaned);
      var chip = cleaned ? data.aiClean : data.aiRaw;
      if (aiChip.textContent !== chip) aiChip.textContent = chip;
      toggle(aiSum, "is-on", aiQ > 0.68);

      var readQ = scene > 5 ? 1 : (scene === 5 ? clamp(q * 1.1) : 0);
      readBody.style.transform = "translateY(" + Math.round(-readQ * 1500) + "px)";
      var tocActive = readQ < 0.42 ? 1 : (readQ < 0.78 ? 2 : 3);
      toc.forEach(function (t, i) { toggle(t, "is-active", i === tocActive); });

      var exQ = scene === 6 ? q : 0;
      var menuHi = exQ < 0.3 ? -1 : 2;
      menu.forEach(function (m, i) { toggle(m, "is-hi", i === menuHi); });
      files.forEach(function (f, i) { toggle(f, "is-on", exQ > 0.35 + i * 0.1); });
    }

    var queued = false;
    function onScroll() {
      if (queued) return;
      queued = true;
      requestAnimationFrame(function () { queued = false; renderStory(); });
    }
    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll);
    renderStory();

    // Steps jump to the middle of their scene, snapped to the design's 40 px sentinels.
    steps.forEach(function (st, i) {
      st.addEventListener("click", function () {
        var m = progress();
        var y = ((i + 0.5) / N) * (m.total - m.pinH);
        var idx = Math.max(0, Math.min(SN - 1, Math.round(y / SG)));
        var top = story.getBoundingClientRect().top + window.scrollY + idx * SG;
        window.scrollTo({ top: top, behavior: reduce ? "auto" : "smooth" });
      });
    });

    // Terminal demo: a 14 s loop ticking every 100 ms (static and complete with reduced motion).
    var term = $("#term");
    var cmdArg = $("#cmd-arg", term);
    var cmdCaret = $("#cmd-caret", term);
    var lines = $$(".t-line", term);
    var done = $("#term-done", term);
    var thresholds = [30, 40, 54, 70, 80];
    var tt = reduce ? 129 : 0;
    function renderTerm() {
      cmdArg.textContent = url.slice(0, Math.floor(clamp((tt - 4) / 20) * url.length));
      cmdCaret.hidden = !(tt < 28);
      lines.forEach(function (l, i) { l.hidden = !(tt >= thresholds[i]); });
      done.hidden = !(tt >= 90);
    }
    renderTerm();
    if (!reduce) {
      setInterval(function () { tt = (tt + 1) % 140; renderTerm(); }, 100);
    }

    // Reveal fallback for browsers without scroll-driven animations.
    if (root.classList.contains("io")) {
      var io = new IntersectionObserver(function (entries) {
        entries.forEach(function (e) {
          if (e.isIntersecting) { e.target.classList.add("seen"); io.unobserve(e.target); }
        });
      }, { threshold: 0.12 });
      $$(".reveal, .reveal-2, .reveal-3, .grow").forEach(function (el) { io.observe(el); });
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
