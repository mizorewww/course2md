// Behaviour of the course2md landing page: the download button for the
// visitor's platform, the scroll-driven "how it works" window, the terminal
// demo, tabs and copy buttons. Strings come from the page's JSON data block
// so one script serves every language.
(function () {
  "use strict";
  var root = document.documentElement;
  root.classList.add("js");

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
    var nav = window.navigator || {};
    try {
      var ua = nav.userAgent || "";
      var plat = (nav.userAgentData && nav.userAgentData.platform) || nav.platform || "";
      var mobile = /Android|iPhone|iPad|iPod|Mobile/i.test(ua) || (nav.userAgentData && nav.userAgentData.mobile);
      if (!mobile) {
        if (/Mac/i.test(plat) || /Macintosh/i.test(ua)) detected = "macos";
        else if (/Win/i.test(plat) || /Windows/i.test(ua)) detected = "windows";
        else if (/Linux|X11/i.test(plat + ua)) detected = "linux";
      }
    } catch (e) {}

    var ctaLink = $("#cta-main");
    var ctaAlt = $("#cta-alt");
    var intelNote = $("#cta-intel");
    function showCta(key) {
      var cta = (data.ctas || {})[key] || (data.ctas || {}).other;
      if (!cta) return;
      ctaLink.setAttribute("href", cta.href);
      $("#cta-label").textContent = cta.label;
      $("#cta-note").textContent = cta.note;
      // When the main button already leads to the download section, a second
      // "Other platforms" button would point to the same place.
      ctaAlt.hidden = cta.href.charAt(0) === "#";
      intelNote.hidden = key !== "macos";
    }
    showCta(detected);
    // Chromium reports the CPU architecture; Intel Macs get the command line.
    if (detected === "macos" && nav.userAgentData && nav.userAgentData.getHighEntropyValues) {
      nav.userAgentData.getHighEntropyValues(["architecture"]).then(function (v) {
        if (v && v.architecture === "x86") { showCta("intelmac"); intelNote.hidden = true; }
      }, function () {});
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
    var steps = $$(".step", story);
    var scenes = $$(".scene", story);
    var N = scenes.length;
    var bar = $(".story-bar span", story);
    var shots = $$(".shot", story);
    var shotCount = $("#shot-count");
    var shotBar = $(".shots-bar span", story);
    var subs = $$(".sub-line", story);
    var spinner = $(".sc-shots .spin", story);
    var doneMark = $(".shots-done", story);
    var menu = $$(".menu-item", story);
    var files = $$(".file", story);
    var last = {};

    function progress() {
      var total = story.offsetHeight;
      var pinH = pin.offsetHeight;
      var top = story.getBoundingClientRect().top;
      return { p: clamp(-top / Math.max(1, total - pinH)), total: total, pinH: pinH };
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

      // Scene 2: screenshots fill in, then the transcript lines arrive.
      var shotQ = scene > 1 ? 1 : (scene === 1 ? clamp(q * 1.8) : 0);
      shots.forEach(function (s, i) { toggle(s, "is-on", shotQ * shots.length > i); });
      shotCount.textContent = String(Math.round(shots.length * shotQ));
      shotBar.style.width = Math.round(shotQ * 100) + "%";
      // The spinner stops once every screenshot is in.
      spinner.toggleAttribute("hidden", shotQ >= 1);
      doneMark.toggleAttribute("hidden", shotQ < 1);
      var subQ = scene > 1 ? 1 : (scene === 1 ? clamp((q - 0.45) * 2.2) : 0);
      subs.forEach(function (s, i) { toggle(s, "is-on", subQ * subs.length > i); });

      // Scene 4: the export menu item is chosen, then the exported files appear.
      var exQ = scene === 3 ? q : (scene > 3 ? 1 : 0);
      var menuHi = exQ < 0.25 ? -1 : 2;
      menu.forEach(function (m, i) { toggle(m, "is-hi", i === menuHi); });
      files.forEach(function (f, i) { toggle(f, "is-on", exQ > 0.35 + i * 0.12); });
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

    // A step jumps to the middle of its scene.
    steps.forEach(function (st, i) {
      st.addEventListener("click", function () {
        var m = progress();
        var y = ((i + 0.5) / N) * (m.total - m.pinH);
        var top = story.getBoundingClientRect().top + window.scrollY + y;
        window.scrollTo({ top: top, behavior: reduce ? "auto" : "smooth" });
      });
    });

    // Terminal demo: a 14 s loop ticking every 100 ms while the terminal is
    // on screen (static and complete with reduced motion).
    var term = $("#term");
    var cmdArg = $("#cmd-arg", term);
    var cmdCaret = $("#cmd-caret", term);
    var lines = $$(".t-line", term);
    var done = $("#term-done", term);
    var start = 30, step = 9;
    var tt = reduce ? 139 : 0;
    var arg = data.cliArg || "";
    function renderTerm() {
      cmdArg.textContent = arg.slice(0, Math.floor(clamp((tt - 4) / 20) * arg.length));
      cmdCaret.hidden = !(tt < 28);
      lines.forEach(function (l, i) { l.hidden = !(tt >= start + i * step); });
      done.hidden = !(tt >= start + lines.length * step + 6);
    }
    renderTerm();
    if (!reduce) {
      var timer = null;
      var run = function (on) {
        if (on && !timer) timer = setInterval(function () { tt = (tt + 1) % 140; renderTerm(); }, 100);
        if (!on && timer) { clearInterval(timer); timer = null; }
      };
      if ("IntersectionObserver" in window) {
        new IntersectionObserver(function (entries) {
          run(entries[entries.length - 1].isIntersecting);
        }).observe(term);
      } else {
        run(true);
      }
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
