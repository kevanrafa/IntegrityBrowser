/* Shared helpers for trusted internal pages (new tab, settings, harness). */
(function () {
  "use strict";

  function send(message) {
    if (window.ipc && typeof window.ipc.postMessage === "function") {
      window.ipc.postMessage(JSON.stringify(message));
      return true;
    }
    return false;
  }

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
  }

  function svg(markup) {
    const span = document.createElement("span");
    span.innerHTML = markup;
    return span.firstElementChild;
  }

  function number(value) {
    const n = Number(value || 0);
    if (!Number.isFinite(n)) return "0";
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(1).replace(/\.0$/, "") + "M";
    if (n >= 10_000) return (n / 1000).toFixed(1).replace(/\.0$/, "") + "k";
    return String(Math.max(0, Math.round(n)));
  }

  function timeAgo(ms) {
    const delta = Math.max(0, Date.now() - Number(ms || 0));
    if (delta < 5000) return "just now";
    if (delta < 60_000) return Math.floor(delta / 1000) + "s ago";
    if (delta < 3_600_000) return Math.floor(delta / 60_000) + "m ago";
    return Math.floor(delta / 3_600_000) + "h ago";
  }

  function toast(message, kind) {
    const stack = document.getElementById("toasts");
    if (!stack) return;
    const item = el("div", "toast" + (kind ? " " + kind : ""), message);
    stack.appendChild(item);
    setTimeout(() => {
      item.classList.add("leaving");
      setTimeout(() => item.remove(), 180);
    }, 3400);
  }

  function setText(id, value) {
    const node = document.getElementById(id);
    if (node) node.textContent = value;
  }

  function setChecked(id, value) {
    const node = document.getElementById(id);
    if (node) node.checked = Boolean(value);
  }

  function getSection() {
    const hash = (location.hash || "").replace(/^#/, "");
    return hash || "general";
  }

  function onSectionChange(callback) {
    window.addEventListener("hashchange", () => callback(getSection()));
    callback(getSection());
  }

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      const search = document.getElementById("newtab-search");
      if (search) {
        search.blur();
      }
    }
  });

  window.__ibPageToast = toast;
  window.IBInternal = {
    send,
    el,
    svg,
    number,
    timeAgo,
    toast,
    setText,
    setChecked,
    getSection,
    onSectionChange,
  };
})();
