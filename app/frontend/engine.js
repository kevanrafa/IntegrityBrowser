/* Integrity Protection engine.
 *
 * Injected at document start into the content webview. It runs in the page's
 * JavaScript world, so everything here is best-effort protection, not a
 * security boundary: a hostile page can inspect and tamper with it. The
 * boundary that matters is the Rust/IPC channel: this script can only report
 * statistics and ask for shortcuts, never change settings.
 *
 * Detection is hybrid:
 *   1. static: large, transparent, pointer-receiving elements stacked above
 *      the page with no visible content;
 *   2. runtime: at gesture time, disarm any such layer under the pointer and
 *      let the real click land on the element below (with a synthetic click
 *      fallback when the browser already fixed the original target).
 *
 * Legitimate transparent elements are preserved by requiring large coverage,
 * low opacity, no visible content, topmost stacking, and absence of dialogs /
 * explicit opt-out markers.
 */
(function () {
  "use strict";

  if (window.top !== window) return; // injected into a subframe (Windows)
  if (window.__IntegrityBrowser) return; // already installed

  // Internal pages never need overlay protection. The self-test page does.
  const path = String(location.pathname || "");
  const internalOrigin =
    location.protocol === "ib:" || location.hostname === "ib.localhost";
  const forced =
    window.__IB_FORCE_ENGINE__ === true ||
    /\/tests\/integrity-harness\.html$/.test(path);
  if (internalOrigin && !forced) return;

  const VERSION = "0.1.0";

  const state = {
    enabled: true,
    clickThrough: true,
    sensitivity: "balanced",
    host: location.hostname || "",
    scanned: 0,
    suspicious: 0,
    neutralized: 0,
    clickThroughs: 0,
    lastAction: "",
  };

  /** @type {Map<Element, object>} */
  const records = new Map();
  const seenElements = new WeakSet();
  let uiHost = null;

  let scanToken = 0;
  let scanTimer = 0;
  let observer = null;
  let lastReportAt = 0;
  let reportTimer = 0;
  let lastToastAt = 0;
  let lastThrough = null;
  let synthetic = false;
  const disarmTimers = new Set();

  /* --------------------------------------------------------------- IPC */

  function send(message) {
    try {
      if (window.ipc && typeof window.ipc.postMessage === "function") {
        window.ipc.postMessage(JSON.stringify(message));
        return true;
      }
    } catch (_err) {
      /* IPC is unavailable in standalone previews. */
    }
    return false;
  }

  function requestConfig(attempt) {
    if (send({ type: "integrity", event: "config_request" })) return;
    if (attempt < 3) {
      setTimeout(() => requestConfig(attempt + 1), 250 * (attempt + 1));
    }
  }

  function doReport() {
    if (reportTimer) {
      clearTimeout(reportTimer);
      reportTimer = 0;
    }
    lastReportAt = Date.now();
    send({
      type: "integrity",
      event: "stats",
      host: state.host || location.hostname || "",
      scanned: state.scanned,
      suspicious: state.suspicious,
      neutralized: state.neutralized,
      clickThroughs: state.clickThroughs,
    });
  }

  function report(force) {
    if (force) {
      doReport();
      return;
    }
    const now = Date.now();
    if (now - lastReportAt >= 350) {
      doReport();
    } else if (!reportTimer) {
      reportTimer = setTimeout(() => {
        reportTimer = 0;
        doReport();
      }, 350);
    }
  }

  function noteAction(action) {
    state.lastAction = action;
    report(true);
  }

  /* ------------------------------------------------------------- toast UI */

  function createToastHost() {
    try {
      const root = document.documentElement || document.body;
      if (!root) return null;
      const host = document.createElement("div");
      host.setAttribute("data-ib-ui", "");
      host.style.cssText =
        "all:initial;position:fixed;right:18px;bottom:18px;z-index:2147483647;" +
        "pointer-events:none;display:block;width:0;height:0;";
      const shadow = host.attachShadow({ mode: "open" });
      const stack = document.createElement("div");
      stack.style.cssText =
        "all:initial;position:fixed;right:18px;bottom:18px;display:flex;" +
        "flex-direction:column;gap:8px;align-items:flex-end;pointer-events:none;" +
        "font-family:system-ui,-apple-system,'Segoe UI',sans-serif;";
      shadow.appendChild(stack);
      root.appendChild(host);
      return host;
    } catch (_err) {
      return null;
    }
  }

  function ensureToastHost() {
    if (uiHost && uiHost.isConnected) return uiHost;
    uiHost = createToastHost();
    return uiHost;
  }

  function toast(message, kind) {
    const host = ensureToastHost();
    if (!host || !host.shadowRoot) return;
    const stack = host.shadowRoot.firstElementChild;
    if (!stack) return;
    const item = document.createElement("div");
    item.textContent = String(message);
    item.style.cssText =
      "all:initial;display:block;max-width:330px;padding:10px 14px;border-radius:10px;" +
      "background:rgba(20,24,32,.97);color:#e9eef7;font-size:12.5px;line-height:1.45;" +
      "font-family:system-ui,-apple-system,'Segoe UI',sans-serif;" +
      "border:1px solid " +
      (kind === "error" ? "rgba(255,131,145,.45)" : "rgba(103,232,195,.38)") +
      ";box-shadow:0 14px 40px rgba(0,0,0,.45);opacity:1;transform:none;";
    stack.appendChild(item);
    setTimeout(() => {
      item.style.opacity = "0";
      item.style.transform = "translateY(4px)";
      setTimeout(() => item.remove(), 200);
    }, 3200);
  }

  function notify(message, kind) {
    const now = Date.now();
    if (now - lastToastAt < 2500) return;
    lastToastAt = now;
    toast(message, kind);
  }

  /* ----------------------------------------------------------- heuristics */

  const SKIP_TAGS = new Set([
    "SCRIPT", "STYLE", "LINK", "META", "TEMPLATE", "NOSCRIPT", "HEAD",
    "TITLE", "BASE", "BR", "HR", "SOURCE", "TRACK", "PARAM", "HTML", "BODY",
  ]);

  const LIMITS = {
    relaxed: { opacity: 0.02, background: 0.03, coverage: 0.9 },
    balanced: { opacity: 0.06, background: 0.06, coverage: 0.55 },
    strict: { opacity: 0.13, background: 0.1, coverage: 0.3 },
  };

  function isOurUi(el) {
    return Boolean(el && el.closest && el.closest("[data-ib-ui]"));
  }

  function isOptedOut(el) {
    return Boolean(
      el.closest &&
        el.closest(
          "[data-ib-legit],[data-integrity-allow],[data-keep-overlay]," +
            "dialog[open],[role='dialog'][aria-modal='true'],[aria-modal='true']"
        )
    );
  }

  function isInert(el) {
    return Boolean(el.closest && el.closest("[inert]"));
  }

  function colorAlpha(color) {
    if (!color) return 1;
    const value = String(color).trim().toLowerCase();
    if (value === "transparent") return 0;
    const match = value.match(/^rgba?\(([^)]+)\)$/);
    if (!match) return 1; // named colour, hex, colour() -> assume opaque
    const parts = match[1].split(/[,\s/]+/).filter(Boolean);
    if (parts.length >= 4) {
      const alpha = parseFloat(parts[3]);
      return Number.isFinite(alpha) ? alpha : 1;
    }
    return 1;
  }

  function coverageOf(rect) {
    const width = window.innerWidth || document.documentElement.clientWidth || 0;
    const height = window.innerHeight || document.documentElement.clientHeight || 0;
    if (!width || !height) return 0;
    const left = Math.max(0, rect.left);
    const top = Math.max(0, rect.top);
    const right = Math.min(width, rect.right);
    const bottom = Math.min(height, rect.bottom);
    if (right <= left || bottom <= top) return 0;
    return ((right - left) * (bottom - top)) / (width * height);
  }

  function visibleMedia(el) {
    if (SKIP_TAGS.has(el.tagName)) return false;
    const nodes = el.querySelectorAll("img,video,canvas,picture,object,embed,svg");
    const max = Math.min(nodes.length, 40);
    for (let i = 0; i < max; i += 1) {
      const node = nodes[i];
      const rect = node.getBoundingClientRect();
      if (rect.width * rect.height < 64) continue;
      const style = getComputedStyle(node);
      if (style.display === "none" || style.visibility === "hidden") continue;
      if (parseFloat(style.opacity) < 0.06) continue;
      return true;
    }
    return false;
  }

  function hasVisibleContent(el) {
    if (el.tagName === "IFRAME" || el.tagName === "FRAME") return false;
    let text = "";
    try {
      text = el.innerText || "";
    } catch (_err) {
      text = el.textContent || "";
    }
    if (text.trim().length > 0) return true;
    return visibleMedia(el);
  }

  function isOpenDialogRelated(el) {
    if (el.closest && el.closest("dialog[open],[role='dialog'],[aria-modal='true']")) {
      return true;
    }
    // A fully transparent backdrop next to a visible open dialog is usually
    // the dialog's click-outside layer, not a click-jacker.
    const parent = el.parentElement;
    if (!parent) return false;
    const siblings = parent.children;
    for (let i = 0; i < siblings.length; i += 1) {
      const sibling = siblings[i];
      if (sibling === el) continue;
      if (!sibling.matches("dialog[open],[role='dialog'],[aria-modal='true']")) continue;
      const rect = sibling.getBoundingClientRect();
      if (rect.width > 20 && rect.height > 20) return true;
    }
    return false;
  }

  function isTopmostAt(el, left, top, right, bottom) {
    const cx = (left + right) / 2;
    const cy = (top + bottom) / 2;
    let hit = null;
    try {
      hit = document.elementFromPoint(cx, cy);
    } catch (_err) {
      return false;
    }
    return Boolean(hit && (hit === el || el.contains(hit)));
  }

  function analyzeElement(el) {
    if (!el || el.nodeType !== 1 || SKIP_TAGS.has(el.tagName)) return null;
    if (isOurUi(el) || isInert(el) || isOptedOut(el) || isOpenDialogRelated(el)) return null;

    const rect = el.getBoundingClientRect();
    if (rect.width < 8 || rect.height < 8) return null;

    const limits = LIMITS[state.sensitivity] || LIMITS.balanced;
    const coverage = coverageOf(rect);
    if (coverage < limits.coverage) return null;

    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden") return null;
    if (style.pointerEvents === "none") return null;
    if (style.position === "static" && style.zIndex === "auto") return null;

    const opacity = parseFloat(style.opacity);
    if (!Number.isFinite(opacity) || opacity > 0.15) return null;

    const faded = opacity <= limits.opacity;
    const seeThrough =
      colorAlpha(style.backgroundColor) <= limits.background &&
      (!style.backgroundImage || style.backgroundImage === "none") &&
      colorAlpha(style.borderTopColor) <= limits.background;
    if (!faded && !seeThrough) return null;

    // Content only counts when it is not itself faded out with the parent.
    if (opacity > 0.12 && hasVisibleContent(el)) return null;

    const width = window.innerWidth || document.documentElement.clientWidth || 0;
    const height = window.innerHeight || document.documentElement.clientHeight || 0;
    const left = Math.max(0, rect.left);
    const top = Math.max(0, rect.top);
    const right = Math.min(width, rect.right);
    const bottom = Math.min(height, rect.bottom);
    if (!isTopmostAt(el, left, top, right, bottom)) return null;

    let record = records.get(el);
    if (!record) {
      record = {
        node: el,
        coverage: coverage,
        opacity: opacity,
        suspicious: false,
        neutralized: false,
        prevPointer: "",
        prevPriority: "",
        tempPointer: "",
        tempPriority: "",
        tempActive: false,
        tempUntil: 0,
      };
      records.set(el, record);
    }
    record.coverage = coverage;
    record.opacity = opacity;
    record.suspicious = true;

    if (
      state.enabled &&
      state.clickThrough &&
      !record.neutralized &&
      qualifiesForNeutralization(record)
    ) {
      neutralize(record);
    }
    return record;
  }

  function qualifiesForNeutralization(record) {
    // Static neutralization is intentionally conservative: it is reserved for
    // layers that cover most of the viewport and are essentially invisible.
    if (state.sensitivity === "relaxed") return record.coverage >= 0.9;
    if (state.sensitivity === "strict") return record.coverage >= 0.35;
    return record.coverage >= 0.55;
  }

  /* ------------------------------------------------------- neutralization */

  function applyPointerNone(node) {
    node.style.setProperty("pointer-events", "none", "important");
  }

  function restorePointer(node, previous) {
    if (previous && previous.value) {
      node.style.setProperty("pointer-events", previous.value, previous.priority);
    } else {
      node.style.removeProperty("pointer-events");
    }
  }

  function neutralize(record) {
    const node = record.node;
    if (!node || !node.isConnected || record.neutralized) return;
    record.prevPointer = node.style.getPropertyValue("pointer-events");
    record.prevPriority = node.style.getPropertyPriority("pointer-events");
    applyPointerNone(node);
    node.setAttribute("data-ib-neutralized", "");
    record.neutralized = true;
    state.neutralized += 1;
    state.lastAction = "neutralized a suspicious overlay";
    if (state.clickThroughs === 0) {
      notify("Integrity Protection neutralized a click-jacking layer.");
    }
    report(true);
    send({ type: "integrity", event: "neutralized" });
    scheduleScan(180);
  }

  function rearm(record) {
    const node = record.node;
    if (!node || !record.tempActive) return;
    record.tempActive = false;
    record.tempUntil = 0;
    if (node.isConnected) {
      restorePointer(node, { value: record.tempPointer, priority: record.tempPriority });
    }
    record.tempPointer = "";
    record.tempPriority = "";
  }

  function disarmForClick(recordsToDisarm) {
    const until = Date.now() + 600;
    recordsToDisarm.forEach((record) => {
      const node = record.node;
      if (!node || !node.isConnected) return;
      if (!record.tempActive) {
        record.tempPointer = node.style.getPropertyValue("pointer-events");
        record.tempPriority = node.style.getPropertyPriority("pointer-events");
        record.tempActive = true;
      }
      record.tempUntil = until;
      applyPointerNone(node);
    });
    const timer = setTimeout(() => {
      disarmTimers.delete(timer);
      recordsToDisarm.forEach((record) => {
        if (record.tempUntil === until && !record.neutralized) rearm(record);
      });
    }, 420);
    disarmTimers.add(timer);
  }

  function restoreAll() {
    disarmTimers.forEach((timer) => clearTimeout(timer));
    disarmTimers.clear();
    records.forEach((record) => {
      if (record.neutralized) {
        restorePointer(record.node, {
          value: record.prevPointer,
          priority: record.prevPriority,
        });
        record.node.removeAttribute("data-ib-neutralized");
        record.neutralized = false;
      }
      if (record.tempActive) rearm(record);
    });
    state.neutralized = 0;
    state.suspicious = 0;
    state.lastAction = "protection disabled; overlays restored";
    report(true);
  }

  /* --------------------------------------------------------------- scan */

  function scheduleScan(delay) {
    if (scanTimer) clearTimeout(scanTimer);
    scanTimer = setTimeout(() => {
      scanTimer = 0;
      runScan();
    }, typeof delay === "number" ? delay : 250);
  }

  function runScan() {
    if (!state.enabled) return;
    const token = ++scanToken;
    const nodes = document.body ? document.body.querySelectorAll("*") : [];
    const total = Math.min(nodes.length, 8000);
    let index = 0;

    function finish() {
      if (token !== scanToken) return;
      let suspicious = 0;
      records.forEach((record, node) => {
        if (!node.isConnected) {
          records.delete(node);
          return;
        }
        if (record.suspicious && !record.neutralized) suspicious += 1;
      });
      state.suspicious = suspicious;
      report(false);
    }

    function step() {
      if (token !== scanToken || !state.enabled) {
        finish();
        return;
      }
      const stop = Math.min(index + 250, total);
      for (; index < stop; index += 1) {
        const node = nodes[index];
        if (!seenElements.has(node)) {
          seenElements.add(node);
          state.scanned = Math.min(1_000_000, state.scanned + 1);
        }
        try {
          analyzeElement(node);
        } catch (_err) {
          /* one malformed element must not stop the scan */
        }
      }
      if (index < total) {
        requestAnimationFrame(step);
      } else {
        finish();
      }
    }

    step();
  }

  /* -------------------------------------------------------- click-through */

  function suspectsFor(target) {
    const found = [];
    let node = target && target.nodeType === 1 ? target : null;
    while (node && node !== document.documentElement) {
      if (isOurUi(node)) break;
      const record = records.get(node);
      if (record && record.suspicious) found.push(record);
      node = node.parentElement;
    }
    return found;
  }

  function elementUnder(clientX, clientY, nodes) {
    const saved = nodes.map((node) => ({
      node: node,
      value: node.style.getPropertyValue("pointer-events"),
      priority: node.style.getPropertyPriority("pointer-events"),
    }));
    nodes.forEach(applyPointerNone);
    let under = null;
    try {
      under = document.elementFromPoint(clientX, clientY);
    } catch (_err) {
      under = null;
    }
    saved.forEach((item) => {
      if (item.value) {
        item.node.style.setProperty("pointer-events", item.value, item.priority);
      } else {
        item.node.style.removeProperty("pointer-events");
      }
    });
    return under;
  }

  function onPointerDown(event) {
    if (!state.enabled || !state.clickThrough) return;
    if (!event.isTrusted || event.button !== 0) return;
    const target = event.target;
    if (!target || target.nodeType !== 1 || isOurUi(target)) return;

    let suspects = suspectsFor(target);
    if (!suspects.length) {
      let node = target;
      for (let depth = 0; node && node !== document.documentElement && depth < 16; depth += 1) {
        const record = analyzeElement(node);
        if (record) {
          suspects = [record];
          break;
        }
        node = node.parentElement;
      }
    }
    if (!suspects.length) return;

    const under = elementUnder(event.clientX, event.clientY, suspects.map((r) => r.node));
    if (
      !under ||
      under === target ||
      isOurUi(under) ||
      suspects.some((record) => record.node === under || record.node.contains(under))
    ) {
      return;
    }

    disarmForClick(suspects);
    lastThrough = { under: under, at: Date.now(), x: event.clientX, y: event.clientY };
    state.clickThroughs += 1;
    state.lastAction = "passed a click through an overlay";
    notify("Click interception blocked. Your click reached the page.");
    send({ type: "integrity", event: "click_through" });
    report(true);
  }

  function syntheticClick(target, event) {
    if (!target || !target.isConnected) return;
    synthetic = true;
    try {
      if (typeof target.click === "function" && target.tagName !== "IFRAME") {
        target.click();
      } else {
        target.dispatchEvent(
          new MouseEvent("click", {
            bubbles: true,
            cancelable: true,
            view: window,
            clientX: event.clientX,
            clientY: event.clientY,
            button: 0,
          })
        );
      }
    } catch (_err) {
      /* ignore */
    }
    setTimeout(() => {
      synthetic = false;
    }, 0);
  }

  function onClick(event) {
    if (synthetic || !state.enabled || !state.clickThrough) return;
    if (!lastThrough || Date.now() - lastThrough.at > 700) return;
    const suspects = suspectsFor(event.target);
    if (!suspects.length) return;
    // The browser already resolved the click to the overlay. Deliver a fallback
    // activation to the element we found underneath.
    event.preventDefault();
    event.stopImmediatePropagation();
    syntheticClick(lastThrough.under, event);
    lastThrough = null;
  }

  /* ------------------------------------------------------------- lifecycle */

  function configure(config) {
    if (!config || typeof config !== "object") return;
    if (typeof config.enabled === "boolean") state.enabled = config.enabled;
    if (typeof config.clickThrough === "boolean") state.clickThrough = config.clickThrough;
    if (typeof config.masterEnabled === "boolean" && typeof config.enabled !== "boolean") {
      state.enabled = config.masterEnabled && !config.siteAllowed;
    }
    if (
      config.sensitivity === "relaxed" ||
      config.sensitivity === "balanced" ||
      config.sensitivity === "strict"
    ) {
      state.sensitivity = config.sensitivity;
    }
    if (typeof config.host === "string") state.host = config.host;
    if (!state.enabled) {
      restoreAll();
      return;
    }
    scheduleScan(60);
    report(true);
  }

  function apiStats() {
    return {
      enabled: state.enabled,
      clickThrough: state.clickThrough,
      sensitivity: state.sensitivity,
      host: state.host || location.hostname || "",
      scanned: state.scanned,
      suspicious: state.suspicious,
      neutralized: state.neutralized,
      clickThroughs: state.clickThroughs,
      lastAction: state.lastAction,
    };
  }

  window.__IntegrityBrowser = {
    version: VERSION,
    configure: configure,
    stats: apiStats,
    scan: function () {
      scheduleScan(0);
    },
    scanNow: runScan,
    toast: toast,
  };

  // Start observing as early as possible so overlays inserted by page scripts
  // are seen even before the document finishes parsing.
  try {
    observer = new MutationObserver(() => {
      if (state.enabled) scheduleScan(220);
    });
    observer.observe(document, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["style", "class", "hidden", "open", "popover"],
    });
  } catch (_err) {
    observer = null;
  }

  window.addEventListener("resize", () => scheduleScan(300), { passive: true });
  window.addEventListener("pointerdown", onPointerDown, true);
  window.addEventListener("click", onClick, true);

  function start() {
    const boot = () => {
      ensureToastHost();
      requestConfig(0);
      scheduleScan(40);
    };
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", boot, { once: true });
    } else {
      boot();
    }
  }

  start();
  // wry installs `window.ipc` before user scripts, so this usually succeeds
  // immediately even while the document is still loading.
  if (document.readyState === "loading") requestConfig(0);
})();
