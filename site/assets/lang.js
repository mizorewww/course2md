// Language choice. Runs in <head> before the page renders.
// The English home page sends a visitor whose browser prefers Chinese to
// /zh/ on their first visit. Picking a language from the header link is
// remembered and wins over the browser; a direct link to /zh/ is never
// redirected. The language links also carry ?lang=en / ?lang=zh so the
// choice holds even where storage is unavailable.
(function () {
  var KEY = "c2m-lang";
  function saved() {
    try { return localStorage.getItem(KEY); } catch (e) { return null; }
  }
  function save(lang) {
    try { localStorage.setItem(KEY, lang); } catch (e) { /* private mode */ }
  }
  function browserPrefersChinese() {
    var langs = navigator.languages && navigator.languages.length
      ? navigator.languages
      : [navigator.language || ""];
    for (var i = 0; i < langs.length; i++) {
      var tag = String(langs[i]).toLowerCase();
      if (tag.indexOf("zh") === 0) return true;
      if (tag.indexOf("en") === 0) return false;
    }
    return false;
  }

  var root = document.documentElement;
  var onEnglishHome = root.lang === "en" && /^\/(index\.html)?$/.test(location.pathname);
  var asked = /(?:^|[?&])lang=(en|zh)(?:&|$)/.exec(location.search);
  if (asked) save(asked[1]);
  var choice = asked ? asked[1] : saved();
  if (onEnglishHome && (choice === "zh" || (!choice && browserPrefersChinese()))) {
    location.replace("/zh/" + location.search + location.hash);
    return;
  }

  // Remember a language picked from the header (or the 404 page).
  document.addEventListener("click", function (event) {
    var link = event.target.closest && event.target.closest("a[hreflang]");
    if (!link) return;
    save(link.getAttribute("hreflang").indexOf("zh") === 0 ? "zh" : "en");
  });
})();
