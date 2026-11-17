/* Integrity Browser chrome UI.
 *
 * Runs inside the trusted chrome webview only. The page webview never gets
 * access to this document; it communicates through the separate content IPC
 * channel and cannot change settings.
 */
(function () {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const address = $("address");
  const addressWrap = $("address-wrap");
  const securityIcon = $("security-icon");
  const hint = $("address-hint");
  const tabsEl = $("tabs");
  const loadBar = $("load-bar");
  const backBtn = $("back");
  const forwardBtn = $("forward");
  const reloadBtn = $("reload");
  const integrityBtn = $("integrity-btn");
  const privacyBtn = $("privacy-btn");
  const integrityCount = $("integrity-count");
  const privacyCount = $("privacy-count");

  let state = null;
  let lastTabsKey = "";
  let focusedInternalUrl = "";

  function send(message) {
    if (window.ipc && typeof window.ipc.postMessage === "function") {
      window.ipc.postMessage(JSON.stringify(message));
      return true;
    }
    return false;
  }

  function command(type, extra) {
    send(Object.assign({ type: type }, extra || {}));
  }

  function internalKind(url) {
    if (!url) return "web";
    const clean = url.split("#")[0].split("?")[0];
    if (clean.endsWith("/newtab.html")) return "newtab";
    if (clean.endsWith("/settings.html")) return "settings";
    if (clean.endsWith("/tests/integrity-harness.html")) return "test";
    if (/^(ib:|http:\/\/ib\.localhost)/.test(url)) return "internal";
    return "web";
  }

  function friendlyUrl(url) {
    switch (internalKind(url)) {
      case "newtab":
        return "New Tab";
      case "settings":
        return "Integrity settings";
      case "test":
        return "Integrity self-test";
      default:
        return url || "";
    }
  }

  function hostOf(url) {
    try {
      return new URL(url).hostname;
    } catch (_err) {
      return "";
    }
  }

  function securityFor(url) {
    if (!url) return { className: "search", hint: "", title: "No page loaded" };
    const kind = internalKind(url);
    if (kind !== "web") {
      return { className: "internal", hint: "Local", title: "Integrity Browser internal page" };
    }
    if (url.startsWith("https://")) {
      return { className: "", hint: "", title: "Connection is encrypted (HTTPS)" };
    }
    if (url.startsWith("http://")) {
      return { className: "insecure", hint: "Not secure", title: "Connection is not encrypted (HTTP)" };
    }
    return { className: "search", hint: "", title: "Address" };
  }

  function initials(title, url) {
    const host = hostOf(url);
    const source = host || title || "?";
    const cleaned = source.replace(/^www\./, "");
    return cleaned.slice(0, 1).toUpperCase();
  }

  function renderTabs() {
    if (!state) return;
    const key = JSON.stringify(state.tabs || []);
    if (key === lastTabsKey) return;
    lastTabsKey = key;

    const fragment = document.createDocumentFragment();
    (state.tabs || []).forEach((tab, index) => {
      const el = document.createElement("div");
      el.className = "tab" + (tab.active ? " active" : "");
      el.setAttribute("role", "tab");
      el.setAttribute("aria-selected", tab.active ? "true" : "false");
      el.tabIndex = 0;
      el.dataset.tabId = String(tab.id);
      el.title = tab.title || tab.url;

      const kind = internalKind(tab.url);
      if (tab.loading && tab.active) {
        const spinner = document.createElement("span");
        spinner.className = "tab-spinner";
        el.appendChild(spinner);
      } else {
        const favicon = document.createElement("span");
        favicon.className = "tab-favicon" + (kind !== "web" ? " internal" : "");
        favicon.textContent = kind === "newtab" ? "N" : initials(tab.title, tab.url);
        el.appendChild(favicon);
      }

      const title = document.createElement("span");
      title.className = "tab-title";
      title.textContent = tab.title || tab.url || "New Tab";
      el.appendChild(title);

      const close = document.createElement("button");
      close.className = "tab-close";
      close.type = "button";
      close.setAttribute("aria-label", "Close tab");
      close.title = "Close tab";
      close.innerHTML =
        '<svg viewBox="0 0 10 10" aria-hidden="true"><path d="M1.6 1.6 8.4 8.4M8.4 1.6 1.6 8.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>';
      close.addEventListener("click", (event) => {
        event.stopPropagation();
        command("close_tab", { id: tab.id });
      });
      el.appendChild(close);

      el.addEventListener("click", () => command("switch_tab", { id: tab.id }));
      el.addEventListener("auxclick", (event) => {
        if (event.button === 1) command("close_tab", { id: tab.id });
      });
      el.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          command("switch_tab", { id: tab.id });
        }
      });

      fragment.appendChild(el);
      if (tab.active) {
        el.dataset.active = "true";
      }
    });

    tabsEl.replaceChildren();
    tabsEl.appendChild(fragment);

    const active = tabsEl.querySelector('.tab[data-active="true"]');
    if (active && typeof active.scrollIntoView === "function") {
      active.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  }

  function renderAddress() {
    if (!state) return;
    const focused = document.activeElement === address;
    if (!focused) {
      address.value = friendlyUrl(state.url);
      address.dataset.url = state.url || "";
      focusedInternalUrl = "";
    } else if (!focusedInternalUrl) {
      // Keep the editable URL visible while the field has focus.
    }

    const security = securityFor(state.url);
    securityIcon.className = "security-icon" + (security.className ? " " + security.className : "");
    securityIcon.title = security.title;
    hint.textContent = security.hint;

    addressWrap.setAttribute("data-kind", internalKind(state.url));
    backBtn.disabled = !state.canGoBack;
    forwardBtn.disabled = !state.canGoForward;
    reloadBtn.title = state.loading ? "Stop loading" : "Reload (Ctrl+R)";
    loadBar.classList.toggle("active", Boolean(state.loading));

    const integrity = state.integrity || {};
    const page = integrity.page || {};
    let badge = "0";
    let badgeClass = "count-badge quiet";
    let pillClass = "tool-pill";

    if (!integrity.masterEnabled) {
      badge = "off";
      pillClass += " off";
    } else if (!integrity.enabled) {
      badge = "paused";
      pillClass += " off";
    } else {
      const suspicious = Number(page.suspicious || 0);
      const neutralized = Number(page.neutralized || 0);
      if (suspicious > 0) {
        badge = String(suspicious);
        badgeClass = "count-badge hot";
        pillClass += " alert";
      } else if (neutralized > 0) {
        badge = String(neutralized);
        badgeClass = "count-badge";
      }
    }
    integrityCount.textContent = badge;
    integrityCount.className = badgeClass;
    integrityBtn.className = pillClass;
    integrityBtn.title =
      "Integrity Protection: " +
      (integrity.enabled
        ? `on (${integrity.sensitivity || "balanced"}). ${Number(page.clickThroughs || 0)} click-through(s).`
        : integrity.masterEnabled
          ? "paused for this site."
          : "off.");

    const stats = (state.privacy || {}).stats || {};
    const blocked = Number(stats.blocked || 0);
    privacyCount.textContent = String(blocked);
    privacyCount.className = "count-badge" + (blocked > 0 ? "" : " quiet");
    privacyBtn.title = `Privacy: ${blocked} request(s) blocked (${Number(stats.ads || 0)} ads, ${Number(
      stats.trackers || 0
    )} trackers)`;
  }

  function render(next) {
    state = next || state;
    if (!state) return;
    renderTabs();
    renderAddress();
  }

  function urlForInput(value) {
    const trimmed = value.trim();
    if (!trimmed) return "";
    const kind = internalKind(trimmed);
    if (kind !== "web" && !trimmed.includes("://") && state) {
      // Friendly labels are never sent as search queries.
      if (trimmed === "New Tab") return "ib://localhost/newtab.html";
      if (trimmed === "Integrity settings") return "ib://localhost/settings.html";
      if (trimmed === "Integrity self-test") {
        return "ib://localhost/tests/integrity-harness.html";
      }
    }
    return trimmed;
  }

  function navigateFromAddress() {
    const input = urlForInput(address.value);
    if (!input) return;
    command("navigate", { input: input });
    address.blur();
  }

  address.addEventListener("focus", () => {
    if (state) {
      const kind = internalKind(state.url);
      if (kind === "newtab" || kind === "settings" || kind === "test") {
        focusedInternalUrl = state.url;
        address.value = state.url;
      }
    }
    requestAnimationFrame(() => address.select());
  });

  address.addEventListener("blur", () => {
    focusedInternalUrl = "";
    if (state) address.value = friendlyUrl(state.url);
  });

  address.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      navigateFromAddress();
    } else if (event.key === "Escape") {
      event.preventDefault();
      address.blur();
    }
  });

  backBtn.addEventListener("click", () => command("back"));
  forwardBtn.addEventListener("click", () => command("forward"));
  reloadBtn.addEventListener("click", () => {
    if (state && state.loading) command("stop");
    else command("reload");
  });
  $("home").addEventListener("click", () => command("home"));
  $("new-tab").addEventListener("click", () => command("new_tab"));
  integrityBtn.addEventListener("click", () => command("open_settings", { section: "integrity" }));
  privacyBtn.addEventListener("click", () => command("open_settings", { section: "privacy" }));
  $("menu-btn").addEventListener("click", () => command("open_settings", { section: "general" }));

  document.addEventListener("keydown", (event) => {
    const ctrl = event.ctrlKey || event.metaKey;
    const alt = event.altKey;
    const key = event.key;
    const lower = key.toLowerCase();

    if (ctrl && !alt && lower === "l") {
      event.preventDefault();
      window.__ibFocusAddress();
    } else if (ctrl && !alt && lower === "t") {
      event.preventDefault();
      command("new_tab");
    } else if (ctrl && !alt && lower === "w") {
      event.preventDefault();
      if (state && state.activeTabId) command("close_tab", { id: state.activeTabId });
    } else if (ctrl && !alt && lower === "r") {
      event.preventDefault();
      command("reload");
    } else if (ctrl && !alt && key === ",") {
      event.preventDefault();
      command("open_settings", { section: "general" });
    } else if (alt && key === "ArrowLeft") {
      event.preventDefault();
      command("back");
    } else if (alt && key === "ArrowRight") {
      event.preventDefault();
      command("forward");
    } else if (key === "F5") {
      event.preventDefault();
      command("reload");
    } else if (key === "F12" || (ctrl && event.shiftKey && lower === "i")) {
      event.preventDefault();
      command("devtools");
    }
  });

  window.__ibUpdate = render;

  window.__ibFocusAddress = function () {
    command("focus_address");
    address.focus();
    if (state) {
      const kind = internalKind(state.url);
      if (kind !== "web") address.value = state.url;
    }
    requestAnimationFrame(() => address.select());
  };

  window.__ibOpenSettings = function (section) {
    command("open_settings", { section: section || "general" });
  };

  // Local preview / development fallback: lets chrome.html render its state in
  // a normal browser tab even when the Rust IPC bridge is unavailable.
  if (!window.ipc) {
    const mock = {
      version: "0.1.0",
      tabs: [
        { id: 1, title: "New Tab", url: "ib://localhost/newtab.html", loading: false, active: true },
        { id: 2, title: "Privacy and performance", url: "https://example.com/privacy", loading: false, active: false },
      ],
      activeTabId: 1,
      url: "ib://localhost/newtab.html",
      loading: false,
      canGoBack: false,
      canGoForward: false,
      integrity: {
        masterEnabled: true,
        enabled: true,
        siteAllowed: false,
        sensitivity: "balanced",
        page: { suspicious: 0, neutralized: 0, clickThroughs: 0 },
      },
      privacy: { stats: { blocked: 0, ads: 0, trackers: 0 } },
    };
    setTimeout(() => render(mock), 30);
  } else {
    command("ready");
  }
})();
