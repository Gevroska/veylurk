(() => {
  "use strict";

  const key = "veylurk.theme";
  const modes = new Set(["auto", "light", "dark"]);
  const normalize = (value) => modes.has(value) ? value : "auto";
  const preference = typeof window.matchMedia === "function" ? window.matchMedia("(prefers-color-scheme: dark)") : null;
  let mode = "auto";
  let selector;
  let message;

  function apply(value) {
    mode = normalize(value);
    document.documentElement.setAttribute("data-theme", mode);
    document.documentElement.setAttribute("data-color-scheme", mode === "auto" ? preference?.matches ? "dark" : "light" : mode);
    if (selector) selector.value = mode;
  }

  try { mode = normalize(window.localStorage.getItem(key)); } catch { /* Auto remains usable without browser storage. */ }
  apply(mode);
  const followSystem = () => { if (mode === "auto") apply("auto"); };
  if (preference?.addEventListener) preference.addEventListener("change", followSystem);
  else if (preference?.addListener) preference.addListener(followSystem);

  function bind() {
    selector = document.getElementById("theme-select");
    message = document.getElementById("theme-message");
    if (!selector) return;
    selector.value = mode;
    selector.addEventListener("change", () => {
      apply(selector.value);
      let saved = true;
      try { window.localStorage.setItem(key, mode); } catch { saved = false; }
      if (message) {
        message.hidden = saved;
        message.textContent = saved ? "" : "Theme changed for this tab. Browser storage is unavailable, so it may reset when you reopen the page.";
      }
    });
  }

  window.addEventListener("storage", (event) => {
    if (event.key !== key && event.key !== null) return;
    try { if (event.storageArea !== window.localStorage) return; } catch { return; }
    apply(event.key === null ? "auto" : event.newValue);
    if (message) { message.hidden = true; message.textContent = ""; }
  });

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", bind, { once: true });
  else bind();
})();
