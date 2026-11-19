/* Settings page controller.
 *
 * Runs in the content webview as a trusted internal page. Every mutation goes
 * through the content IPC bridge, which only accepts settings commands while
 * an internal page is active.
 */
(function () {
  "use strict";

  const I = window.IBInternal;
  const SECTIONS = {
    general: {
      title: "General",
      subtitle: "Browser status and session summary.",
    },
    privacy: {
      title: "Privacy",
      subtitle: "Ad and tracker blocking, custom rules and filter lists.",
    },
    integrity: {
      title: "Integrity Protection",
      subtitle: "Overlay detection, click interception and per-site pauses.",
    },
    filters: {
      title: "Filter lists",
      subtitle: "Built-in rules plus host lists you add yourself.",
    },
    about: {
      title: "About",
      subtitle: "How Integrity Browser is built and what it cannot do yet.",
    },
  };

  const navButtons = Array.from(document.querySelectorAll("#settings-nav [data-section]"));
  const panels = Array.from(document.querySelectorAll("[data-panel]"));
  const titleEl = document.getElementById("section-title");
  const subtitleEl = document.getElementById("section-subtitle");
  const warningArea = document.getElementById("warning-area");
  const remoteNote = document.getElementById("remote-note");
  const recentList = document.getElementById("recent-list");
  const customList = document.getElementById("custom-list");
  const filterListsEl = document.getElementById("filter-lists");
  const customHost = document.getElementById("custom-host");
  const addHostBtn = document.getElementById("add-host");
  const remoteUrl = document.getElementById("remote-url");
  const addRemoteBtn = document.getElementById("add-remote");
  const refreshBtn = document.getElementById("refresh-lists");
  const siteAllow = document.getElementById("site-allow");
  const siteHost = document.getElementById("site-host");
  const siteChip = document.getElementById("site-chip");

  let state = null;
  let suppress = false;
  let currentSection = "";

  /* ------------------------------------------------------------- utilities */

  function send(type, extra) {
    return I.send(Object.assign({ type: type }, extra || {}));
  }

  function safeUrl(value) {
    const raw = String(value || "").trim();
    if (!/^https?:\/\//i.test(raw)) return "";
    try {
      const parsed = new URL(raw);
      return parsed.protocol === "http:" || parsed.protocol === "https:" ? parsed.toString() : "";
    } catch (_err) {
      return "";
    }
  }

  function setToggle(id, value) {
    const node = document.getElementById(id);
    if (node && typeof value === "boolean") node.checked = value;
  }

  function setChip(id, text, kind) {
    const node = document.getElementById(id);
    if (!node) return;
    node.textContent = text;
    node.className = "chip" + (kind ? " " + kind : "");
  }

  function emptyRow(text) {
    const div = document.createElement("div");
    div.className = "empty";
    div.textContent = text;
    return div;
  }

  function listItem(title, sub) {
    const item = document.createElement("div");
    item.className = "list-item";
    const main = document.createElement("div");
    main.className = "list-main";
    const titleEl = document.createElement("div");
    titleEl.className = "list-title";
    titleEl.textContent = title;
    main.appendChild(titleEl);
    if (sub) {
      const subEl = document.createElement("div");
      subEl.className = "list-sub";
      subEl.textContent = sub;
      main.appendChild(subEl);
    }
    item.appendChild(main);
    return item;
  }

  function replaceList(node, children) {
    node.replaceChildren.apply(node, children);
  }

  /* -------------------------------------------------------------- sections */

  function sectionFromHash() {
    const hash = (location.hash || "").replace(/^#/, "").split("?")[0];
    return Object.prototype.hasOwnProperty.call(SECTIONS, hash) ? hash : "general";
  }

  function showSection(name, updateHash) {
    const section = SECTIONS[name] ? name : "general";
    currentSection = section;
    const meta = SECTIONS[section];
    titleEl.textContent = meta.title;
    subtitleEl.textContent = meta.subtitle;

    navButtons.forEach((button) => {
      const active = button.dataset.section === section;
      button.classList.toggle("active", active);
      button.setAttribute("aria-current", active ? "page" : "false");
    });
    panels.forEach((panel) => {
      panel.classList.toggle("hidden", panel.dataset.panel !== section);
    });
    document.querySelector(".content").scrollTop = 0;

    if (updateHash !== false && sectionFromHash() !== section) {
      try {
        history.replaceState(null, "", "#" + section);
      } catch (_err) {
        /* replaceState is unavailable on some file origins; ignore. */
      }
    }
    if (section === "privacy") refreshFilterCaches();
  }

  navButtons.forEach((button) => {
    button.addEventListener("click", () => showSection(button.dataset.section));
  });
  window.addEventListener("hashchange", () => showSection(sectionFromHash(), false));

  /* ---------------------------------------------------------------- toggles */

  const toggleMap = {
    "adblock-toggle": ["toggle_adblock", "enabled"],
    "tracker-toggle": ["toggle_tracker_blocking", "enabled"],
    "integrity-toggle": ["toggle_integrity", "enabled"],
    "click-through-toggle": ["toggle_click_through", "enabled"],
    "hidden-tabs-toggle": ["toggle_block_hidden_tabs", "enabled"],
  };

  Object.keys(toggleMap).forEach((id) => {
    const node = document.getElementById(id);
    if (!node) return;
    node.addEventListener("change", () => {
      if (suppress) return;
      send(toggleMap[id][0], { enabled: node.checked });
    });
  });

  Array.from(document.querySelectorAll("#sensitivity [data-sensitivity]")).forEach((button) => {
    button.addEventListener("click", () => {
      const sensitivity = button.dataset.sensitivity;
      if (!sensitivity) return;
      setSensitivity(sensitivity);
      send("set_sensitivity", { sensitivity: sensitivity });
    });
  });

  function setSensitivity(value) {
    Array.from(document.querySelectorAll("#sensitivity [data-sensitivity]")).forEach((button) => {
      button.classList.toggle("active", button.dataset.sensitivity === value);
    });
  }

  siteAllow.addEventListener("change", () => {
    if (suppress) return;
    const target = state && state.integrity ? state.integrity.targetHost : "";
    if (!target) {
      siteAllow.checked = false;
      return;
    }
    send("toggle_site_allowlist", { allowed: siteAllow.checked });
  });

  document.getElementById("reset-stats").addEventListener("click", () => {
    send("reset_stats");
  });

  /* ------------------------------------------------------------ custom hosts */

  function addCustomHost() {
    const host = customHost.value.trim();
    if (!host) {
      customHost.focus();
      return;
    }
    send("add_custom_host", { host: host });
    customHost.value = "";
  }

  addHostBtn.addEventListener("click", addCustomHost);
  customHost.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      addCustomHost();
    }
  });

  /* ------------------------------------------------------------ remote lists */

  function addRemoteList() {
    const url = safeUrl(remoteUrl.value);
    if (!url) {
      I.toast("Enter an http:// or https:// list URL.", "error");
      remoteUrl.focus();
      return;
    }
    send("add_remote_list", { url: url });
    remoteUrl.value = "";
  }

  addRemoteBtn.addEventListener("click", addRemoteList);
  remoteUrl.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      addRemoteList();
    }
  });

  refreshBtn.addEventListener("click", () => {
    send("refresh_filter_lists");
  });

  /* --------------------------------------------------------------- actions */

  ["general-test", "integrity-test"].forEach((id) => {
    const node = document.getElementById(id);
    if (node) node.addEventListener("click", () => send("open_test_page"));
  });
  document.getElementById("general-privacy").addEventListener("click", () => {
    showSection("privacy");
  });

  ["done-side", "done-top"].forEach((id) => {
    const node = document.getElementById(id);
    if (node) node.addEventListener("click", () => send("close_settings"));
  });

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      send("close_settings");
    }
  });

  /* ---------------------------------------------------------------- render */

  const cacheKeys = { recent: "", custom: "", lists: "" };

  function renderStats(prefix, stats) {
    I.setText(prefix + "-blocked", I.number(stats.blocked));
    I.setText(prefix + "-ads", I.number(stats.ads));
    I.setText(prefix + "-trackers", I.number(stats.trackers));
    I.setText(prefix + "-inspected", I.number(stats.inspected));
  }

  function renderRecent(items) {
    const key = JSON.stringify(items || []);
    if (key === cacheKeys.recent) return;
    cacheKeys.recent = key;
    if (!items || !items.length) {
      replaceList(recentList, [emptyRow("Nothing blocked yet in this session.")]);
      return;
    }
    const nodes = items.slice(0, 20).map((item) => {
      const when = item.atMs ? I.timeAgo(item.atMs) : "";
      return listItem(item.host || "unknown host", [item.category, when].filter(Boolean).join(" · "));
    });
    replaceList(recentList, nodes);
  }

  function renderCustomHosts(hosts) {
    const key = JSON.stringify(hosts || []);
    if (key === cacheKeys.custom) return;
    cacheKeys.custom = key;
    if (!hosts || !hosts.length) {
      replaceList(customList, [emptyRow("No custom hosts.")]);
      return;
    }
    const nodes = hosts.map((host) => {
      const item = listItem(host, "Blocked at the connection level");
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "btn small ghost";
      remove.textContent = "Remove";
      remove.addEventListener("click", () => send("remove_custom_host", { host: host }));
      item.appendChild(remove);
      return item;
    });
    replaceList(customList, nodes);
  }

  function renderFilterLists(lists) {
    const key = JSON.stringify(lists || []);
    if (key === cacheKeys.lists) return;
    cacheKeys.lists = key;
    if (!lists || !lists.length) {
      replaceList(filterListsEl, [emptyRow("Loading filter lists…")]);
      return;
    }
    const nodes = lists.map((list) => {
      const builtin = list.kind === "builtin_ads" || list.kind === "builtin_trackers";
      const rules = Number(list.ruleCount || 0);
      const notes = [];
      if (rules > 0) notes.push(rules.toLocaleString() + " rules");
      if (list.kind === "builtin_ads") notes.push("ads");
      if (list.kind === "builtin_trackers") notes.push("trackers");
      if (list.url) notes.push(list.url);
      if (list.kind === "remote" && !list.cached) notes.push("not downloaded");

      const item = listItem(list.name || list.id, notes.join(" · "));
      const label = document.createElement("label");
      label.className = "switch";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = Boolean(list.enabled);
      input.disabled = builtin ? false : !list.cached;
      input.setAttribute("aria-label", "Enable " + (list.name || list.id));
      input.addEventListener("change", () => {
        send("toggle_filter_list", { id: list.id, enabled: input.checked });
      });
      const knob = document.createElement("span");
      label.appendChild(input);
      label.appendChild(knob);
      item.appendChild(label);

      if (!builtin) {
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "btn small ghost";
        remove.textContent = "Remove";
        remove.addEventListener("click", () => send("remove_filter_list", { id: list.id }));
        item.appendChild(remove);
      }
      return item;
    });
    replaceList(filterListsEl, nodes);
  }

  function renderWarnings(warnings) {
    if (!warnings || !warnings.length) {
      warningArea.classList.add("hidden");
      warningArea.replaceChildren();
      return;
    }
    warningArea.classList.remove("hidden");
    const nodes = warnings.map((warning) => {
      const div = document.createElement("div");
      div.className = "notice";
      div.textContent = warning;
      return div;
    });
    warningArea.replaceChildren.apply(warningArea, nodes);
  }

  function renderSite() {
    const integrity = state.integrity || {};
    const target = integrity.targetHost || "";
    const paused = Boolean(integrity.targetSiteAllowed);
    siteHost.textContent = target || "No website loaded yet";
    siteAllow.disabled = !target;
    siteAllow.checked = target ? paused : false;
    if (!target) {
      setChip("site-chip", "no site", "");
    } else if (paused) {
      setChip("site-chip", "paused", "warn");
    } else {
      setChip("site-chip", "protected", "on");
    }
  }

  function renderPage(integrity) {
    const page = integrity.page || {};
    I.setText("i-scanned", I.number(page.scanned));
    I.setText("i-suspicious", I.number(page.suspicious));
    I.setText("i-neutralized", I.number(page.neutralized));
    I.setText("i-through", I.number(page.clickThroughs));
    I.setText("i-hidden-tabs", I.number(page.hiddenTabsBlocked));
    I.setText(
      "page-last-action",
      page.lastAction && page.lastAction.trim() ? page.lastAction : "No action taken."
    );
  }

  function render() {
    if (!state) return;
    const privacy = state.privacy || {};
    const stats = privacy.stats || {};
    const integrity = state.integrity || {};

    suppress = true;
    setToggle("adblock-toggle", privacy.adblockEnabled);
    setToggle("tracker-toggle", privacy.trackerBlockingEnabled);
    setToggle("integrity-toggle", integrity.masterEnabled);
    setToggle("click-through-toggle", integrity.clickThrough);
    setToggle("hidden-tabs-toggle", integrity.blockHiddenTabs);
    suppress = false;

    setChip("adblock-chip", privacy.adblockEnabled ? "on" : "off", privacy.adblockEnabled ? "on" : "off");
    setChip(
      "tracker-chip",
      privacy.trackerBlockingEnabled ? "on" : "off",
      privacy.trackerBlockingEnabled ? "on" : "off"
    );
    setChip(
      "integrity-chip",
      integrity.masterEnabled ? "on" : "off",
      integrity.masterEnabled ? "on" : "off"
    );
    setChip(
      "hidden-tabs-chip",
      integrity.blockHiddenTabs ? "on" : "off",
      integrity.blockHiddenTabs ? "on" : "off"
    );

    setSensitivity(integrity.sensitivity || "balanced");

    const filtering = stats.enabled !== false;
    setChip("proxy-status", filtering ? "Filter proxy active" : "Filtering paused", filtering ? "on" : "warn");
    I.setText("proxy-url", privacy.proxyUrl || "http://127.0.0.1:…");

    I.setText("version-chip", state.version || "0.1.0");
    I.setText("about-version", state.version || "0.1.0");

    renderStats("g", stats);
    renderStats("p", stats);
    renderRecent(stats.recent);
    renderCustomHosts(privacy.customHosts);
    renderFilterLists(privacy.lists);
    renderWarnings(state.warnings);
    renderSite();
    renderPage(integrity);

    const note = String(state.remoteNote || "").trim();
    remoteNote.hidden = !note;
    remoteNote.className = "notice info";
    remoteNote.textContent = note;
  }

  window.__ibPageUpdate = function (next) {
    if (!next || typeof next !== "object") return;
    state = next;
    render();
  };

  // Refresh the filter-list cache keys after a list is added so the next
  // snapshot re-renders even if only rule counts changed.
  function refreshFilterCaches() {
    cacheKeys.lists = "";
  }

  /* -------------------------------------------------------------- standalone */

  if (!window.ipc) {
    // Preview fallback for opening settings.html directly in a browser.
    state = {
      version: "0.1.0",
      host: "",
      integrity: {
        masterEnabled: true,
        clickThrough: true,
        blockHiddenTabs: false,
        sensitivity: "balanced",
        targetHost: "",
        targetSiteAllowed: false,
        page: {
          scanned: 0,
          suspicious: 0,
          neutralized: 0,
          clickThroughs: 0,
          hiddenTabsBlocked: 0,
          lastAction: "",
        },
      },
      privacy: {
        adblockEnabled: true,
        trackerBlockingEnabled: true,
        proxyUrl: "http://127.0.0.1:0 (preview)",
        stats: {
          blocked: 0,
          ads: 0,
          trackers: 0,
          inspected: 0,
          enabled: true,
          recent: [],
        },
        lists: [
          { id: "builtin-ads", name: "Integrity Ads (built-in)", enabled: true, kind: "builtin_ads", ruleCount: 0, cached: true },
          { id: "builtin-trackers", name: "Integrity Trackers (built-in)", enabled: true, kind: "builtin_trackers", ruleCount: 0, cached: true },
        ],
        customHosts: [],
      },
      warnings: [],
      remoteNote: "",
    };
    I.toast("Preview mode: Rust IPC is not available.", "info");
  }

  showSection(sectionFromHash(), false);
  render();
  if (window.ipc) send("refresh_state");
})();
