// Theme toggle and copy buttons. Loaded synchronously in <head> so a saved
// theme applies before the first paint; everything else waits for the DOM.
(function () {
  var root = document.documentElement;
  var KEY = "course2md-theme";
  var media = window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : null;

  function load() {
    try {
      var value = window.localStorage.getItem(KEY);
      return value === "light" || value === "dark" ? value : null;
    } catch (e) {
      return null;
    }
  }

  function save(value) {
    try {
      window.localStorage.setItem(KEY, value);
    } catch (e) {
      /* storage unavailable: the choice lasts for this page view only */
    }
  }

  function scheme() {
    var forced = root.getAttribute("data-theme");
    if (forced) return forced;
    return media && media.matches ? "dark" : "light";
  }

  // <picture> sources follow the system scheme by default; a manual choice
  // rewrites their media query so screenshots match the page.
  function syncPictures() {
    var forced = root.getAttribute("data-theme");
    var query = forced === "dark" ? "all" : forced === "light" ? "not all" : "(prefers-color-scheme: dark)";
    var sources = document.querySelectorAll("source[data-dark]");
    for (var i = 0; i < sources.length; i++) {
      if (sources[i].media !== query) sources[i].media = query;
    }
  }

  function syncButton() {
    var button = document.getElementById("theme-toggle");
    if (!button) return;
    var dark = scheme() === "dark";
    var label = button.getAttribute(dark ? "data-label-light" : "data-label-dark");
    button.setAttribute("aria-label", label);
    button.setAttribute("title", label);
  }

  function apply() {
    root.setAttribute("data-scheme", scheme());
    syncPictures();
    syncButton();
  }

  var saved = load();
  if (saved) root.setAttribute("data-theme", saved);
  root.setAttribute("data-scheme", scheme());

  if (media) {
    var onChange = function () {
      if (!root.getAttribute("data-theme")) apply();
    };
    if (media.addEventListener) media.addEventListener("change", onChange);
    else if (media.addListener) media.addListener(onChange);
  }

  function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) {
      return navigator.clipboard.writeText(text);
    }
    return new Promise(function (resolve, reject) {
      var area = document.createElement("textarea");
      area.value = text;
      area.setAttribute("readonly", "");
      area.className = "visually-hidden";
      document.body.appendChild(area);
      area.select();
      var ok = false;
      try {
        ok = document.execCommand("copy");
      } catch (e) {
        ok = false;
      }
      document.body.removeChild(area);
      ok ? resolve() : reject();
    });
  }

  function init() {
    apply();

    var toggle = document.getElementById("theme-toggle");
    if (toggle) {
      toggle.hidden = false;
      toggle.addEventListener("click", function () {
        var next = scheme() === "dark" ? "light" : "dark";
        root.setAttribute("data-theme", next);
        save(next);
        apply();
      });
    }

    var status = document.getElementById("copy-status");
    var buttons = document.querySelectorAll("button.copy");
    for (var i = 0; i < buttons.length; i++) {
      buttons[i].hidden = false;
      buttons[i].addEventListener("click", function (event) {
        var button = event.currentTarget;
        var code = button.parentNode.querySelector("code");
        if (!code) return;
        var label = button.querySelector(".label");
        var idle = button.getAttribute("data-idle");
        var line = code.querySelector(".line") || code;
        copyText(line.textContent.trim()).then(function () {
          button.classList.add("done");
          var done = button.getAttribute("data-done");
          if (label) label.textContent = done;
          if (status) status.textContent = done;
          clearTimeout(button._reset);
          button._reset = setTimeout(function () {
            button.classList.remove("done");
            if (label) label.textContent = idle;
            if (status) status.textContent = "";
          }, 2000);
        }, function () {
          var range = document.createRange();
          range.selectNodeContents(line);
          var selection = window.getSelection();
          selection.removeAllRanges();
          selection.addRange(range);
        });
      });
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
