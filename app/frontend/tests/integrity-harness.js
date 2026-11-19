/* Integrity self-test harness.
 *
 * Creates real overlays in this page and asks the injected engine to deal with
 * them. Runs both in the browser (internal test page) and as a plain file when
 * opened directly in any modern browser.
 */
(function () {
  "use strict";

  // The engine is injected at document start inside the app. When this file is
  // opened directly, the engine script below this one checks this flag so it
  // still runs on the (non-internal) file:// origin.
  window.__IB_FORCE_ENGINE__ = true;

  const I = window.IBInternal;
  const cardsEl = document.getElementById("checks");
  const banner = document.getElementById("engine-banner");
  const bannerText = document.getElementById("engine-text");
  const summary = document.getElementById("summary");
  const runBtn = document.getElementById("run-checks");
  const clearBtn = document.getElementById("clear-results");

  let running = false;

  const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  function waitFor(check, timeout) {
    const end = Date.now() + timeout;
    return new Promise((resolve) => {
      (function poll() {
        let ok = false;
        try {
          ok = check();
        } catch (_err) {
          ok = false;
        }
        if (ok) {
          resolve(true);
        } else if (Date.now() >= end) {
          resolve(false);
        } else {
          setTimeout(poll, 120);
        }
      })();
    });
  }

  function neutralized(node) {
    return Boolean(node && node.hasAttribute("data-ib-neutralized"));
  }

  function layer(styles) {
    const node = document.createElement("div");
    node.setAttribute("data-ib-scenario", "");
    node.style.cssText = styles;
    document.body.appendChild(node);
    return node;
  }

  function dialogBox(text) {
    const node = document.createElement("div");
    node.setAttribute("data-ib-scenario", "");
    node.setAttribute("role", "dialog");
    node.setAttribute("aria-modal", "true");
    node.textContent = text;
    node.style.cssText =
      "position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);" +
      "width:min(420px,80vw);padding:22px;border-radius:14px;background:#1b2130;" +
      "color:#e9eef7;border:1px solid rgba(255,255,255,.16);z-index:2147482905;" +
      "font:13px/1.5 system-ui,sans-serif;box-shadow:0 18px 50px rgba(0,0,0,.45)";
    document.body.appendChild(node);
    return node;
  }

  function cleanup() {
    document.querySelectorAll("[data-ib-scenario]").forEach((node) => node.remove());
  }

  function engineStats() {
    const engine = window.__IntegrityBrowser;
    return engine && typeof engine.stats === "function" ? engine.stats() : {};
  }

  function hiddenAnchor() {
    const anchor = document.createElement("a");
    anchor.setAttribute("data-ib-scenario", "");
    anchor.href = "about:blank";
    anchor.target = "_blank";
    anchor.style.cssText =
      "position:fixed;inset:0;z-index:2147483000;background:transparent;" +
      "opacity:0.001;pointer-events:auto";
    return anchor;
  }

  const CHECKS = [
    {
      id: "clickjacker",
      title: "Transparent full-screen click-jacker",
      expect: "A near-invisible layer covering the viewport is neutralized.",
      positive: true,
      setup() {
        const node = layer(
          "position:fixed;inset:0;z-index:2147483000;background:transparent;" +
            "opacity:0.001;pointer-events:auto"
        );
        return { nodes: [node] };
      },
      verify(refs) {
        return neutralized(refs.nodes[0]);
      },
      timeout: 4000,
    },
    {
      id: "stacked",
      title: "Stacked transparent overlays",
      expect: "Both layers are peeled, including the one revealed underneath.",
      positive: true,
      setup() {
        const bottom = layer(
          "position:fixed;inset:0;z-index:2147483001;background:transparent;opacity:0.001"
        );
        const top = layer(
          "position:fixed;inset:0;z-index:2147483002;background:transparent;opacity:0.001"
        );
        return { nodes: [top, bottom] };
      },
      verify(refs) {
        return neutralized(refs.nodes[0]) && neutralized(refs.nodes[1]);
      },
      timeout: 6000,
    },
    {
      id: "iframe",
      title: "Invisible full-page iframe",
      expect: "A transparent iframe covering the page is neutralized.",
      positive: true,
      setup() {
        const frame = document.createElement("iframe");
        frame.setAttribute("data-ib-scenario", "");
        frame.setAttribute("title", "invisible frame");
        frame.style.cssText =
          "position:fixed;inset:0;width:100vw;height:100vh;border:0;" +
          "z-index:2147483000;opacity:0";
        document.body.appendChild(frame);
        return { nodes: [frame] };
      },
      verify(refs) {
        return neutralized(refs.nodes[0]);
      },
      timeout: 4000,
    },
    {
      id: "backdrop",
      title: "Legitimate transparent modal backdrop",
      expect: "Left alone: it belongs to an open, visible dialog.",
      positive: false,
      settle: 1500,
      setup() {
        const backdrop = layer(
          "position:fixed;inset:0;z-index:2147482900;background:transparent"
        );
        const dialog = dialogBox("Legitimate dialog. This backdrop must keep working.");
        return { nodes: [backdrop, dialog] };
      },
      verify(refs) {
        return !neutralized(refs.nodes[0]) && !neutralized(refs.nodes[1]);
      },
    },
    {
      id: "modal",
      title: "Visible modal with a dim backdrop",
      expect: "Left alone: the backdrop is visible and the dialog has content.",
      positive: false,
      settle: 1500,
      setup() {
        const backdrop = layer(
          "position:fixed;inset:0;z-index:2147482900;background:rgba(0,0,0,.55)"
        );
        const dialog = dialogBox("Visible modal. Nothing here should be touched.");
        return { nodes: [backdrop, dialog] };
      },
      verify(refs) {
        return !neutralized(refs.nodes[0]) && !neutralized(refs.nodes[1]);
      },
    },
    {
      id: "badge",
      title: "Small transparent badge",
      expect: "Left alone: it is too small to be a click-jacking layer.",
      positive: false,
      settle: 1500,
      setup() {
        const badge = layer(
          "position:fixed;left:70px;top:70px;width:120px;height:28px;z-index:2147483000;" +
            "background:transparent;opacity:0.001"
        );
        badge.textContent = "";
        return { nodes: [badge] };
      },
      verify(refs) {
        return !neutralized(refs.nodes[0]);
      },
    },
    {
      id: "content",
      title: "Large layer with visible content",
      expect: "Left alone: transparency alone is not suspicious.",
      positive: false,
      settle: 1500,
      setup() {
        const panel = layer(
          "position:fixed;left:10%;top:10%;width:80%;height:80%;z-index:2147482900;" +
            "background:transparent;color:#e9eef7;font:14px/1.5 system-ui,sans-serif;" +
            "display:flex;align-items:center;justify-content:center;text-align:center"
        );
        panel.textContent = "Legitimate site panel with visible content.";
        return { nodes: [panel] };
      },
      verify(refs) {
        return !neutralized(refs.nodes[0]);
      },
    },
    {
      id: "reach",
      title: "Click reaches the real control",
      expect: "After the overlay is neutralized, the button underneath is clickable.",
      positive: true,
      setup() {
        let clicks = 0;
        const button = document.createElement("button");
        button.type = "button";
        button.setAttribute("data-ib-scenario", "");
        button.textContent = "Real control";
        button.style.cssText =
          "position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);" +
          "width:150px;height:44px;z-index:2147482000;border-radius:9px;border:0;" +
          "background:#67e8c3;color:#07110f;font:600 13px system-ui,sans-serif";
        button.addEventListener("click", () => {
          clicks += 1;
        });
        document.body.appendChild(button);
        const overlay = layer(
          "position:fixed;inset:0;z-index:2147483000;background:transparent;opacity:0.001"
        );
        return { nodes: [button, overlay], clicks: () => clicks };
      },
      verify(refs) {
        if (!neutralized(refs.nodes[1])) return false;
        const rect = refs.nodes[0].getBoundingClientRect();
        const hit = document.elementFromPoint(
          rect.left + rect.width / 2,
          rect.top + rect.height / 2
        );
        if (!hit || !(hit === refs.nodes[0] || refs.nodes[0].contains(hit))) return false;
        refs.nodes[0].click();
        return refs.clicks() === 1;
      },
      timeout: 4000,
    },
    {
      id: "hidden-blank",
      title: "Invisible link that opens a new tab",
      expect: "The click reaches the page, but navigation is cancelled.",
      positive: true,
      setup() {
        const anchor = hiddenAnchor();
        let clicks = 0;
        let blockedByEngine = false;
        const before = Number(engineStats().hiddenTabsBlocked || 0);
        anchor.addEventListener("click", (event) => {
          clicks += 1;
          blockedByEngine = event.defaultPrevented;
          event.preventDefault();
        });
        document.body.appendChild(anchor);
        anchor.click();
        return {
          nodes: [anchor],
          clicks: () => clicks,
          blocked: () => blockedByEngine,
          before: before,
        };
      },
      verify(refs) {
        return (
          refs.clicks() === 1 &&
          refs.blocked() &&
          Number(engineStats().hiddenTabsBlocked || 0) > refs.before
        );
      },
      timeout: 4000,
    },
    {
      id: "visible-blank",
      title: "Visible link that opens a new tab",
      expect: "Left alone: only invisible links are cancelled.",
      positive: false,
      settle: 250,
      setup() {
        const anchor = document.createElement("a");
        anchor.setAttribute("data-ib-scenario", "");
        anchor.href = "about:blank";
        anchor.target = "_blank";
        anchor.textContent = "Visible link";
        anchor.style.cssText =
          "position:fixed;left:24px;top:24px;z-index:2147483000;padding:8px 12px;" +
          "background:#1b2130;color:#e9eef7;font:13px system-ui,sans-serif";
        let clicks = 0;
        let blockedByEngine = false;
        const before = Number(engineStats().hiddenTabsBlocked || 0);
        anchor.addEventListener("click", (event) => {
          clicks += 1;
          blockedByEngine = event.defaultPrevented;
          event.preventDefault();
        });
        document.body.appendChild(anchor);
        anchor.click();
        return {
          nodes: [anchor],
          clicks: () => clicks,
          blocked: () => blockedByEngine,
          before: before,
        };
      },
      verify(refs) {
        return (
          refs.clicks() === 1 &&
          !refs.blocked() &&
          Number(engineStats().hiddenTabsBlocked || 0) === refs.before
        );
      },
    },
    {
      id: "modified-click",
      title: "Modified click on an invisible link",
      expect: "Left alone: Ctrl keeps the browser's own new-tab behaviour.",
      positive: false,
      settle: 250,
      setup() {
        const anchor = hiddenAnchor();
        let clicks = 0;
        let blockedByEngine = false;
        const before = Number(engineStats().hiddenTabsBlocked || 0);
        anchor.addEventListener("click", (event) => {
          clicks += 1;
          blockedByEngine = event.defaultPrevented;
          event.preventDefault();
        });
        document.body.appendChild(anchor);
        anchor.dispatchEvent(
          new MouseEvent("click", {
            bubbles: true,
            cancelable: true,
            ctrlKey: true,
            button: 0,
          })
        );
        return {
          nodes: [anchor],
          clicks: () => clicks,
          blocked: () => blockedByEngine,
          before: before,
        };
      },
      verify(refs) {
        return (
          refs.clicks() === 1 &&
          !refs.blocked() &&
          Number(engineStats().hiddenTabsBlocked || 0) === refs.before
        );
      },
    },
    {
      id: "hidden-open",
      title: "Invisible link using window.open",
      expect: "The handler runs, but window.open does not create a real window.",
      positive: true,
      setup() {
        const anchor = hiddenAnchor();
        let clicks = 0;
        let blockedByEngine = false;
        let opened = null;
        const before = Number(engineStats().hiddenTabsBlocked || 0);
        anchor.addEventListener("click", (event) => {
          clicks += 1;
          blockedByEngine = event.defaultPrevented;
          opened = window.open("about:blank");
          event.preventDefault();
        });
        document.body.appendChild(anchor);
        anchor.click();
        return {
          nodes: [anchor],
          clicks: () => clicks,
          blocked: () => blockedByEngine,
          opened: () => opened,
          before: before,
        };
      },
      verify(refs) {
        const opened = refs.opened();
        const stubbed = Boolean(opened && !("top" in opened));
        return (
          refs.clicks() === 1 &&
          refs.blocked() &&
          stubbed &&
          Number(engineStats().hiddenTabsBlocked || 0) > refs.before
        );
      },
      timeout: 4000,
    },
  ];

  /* ----------------------------------------------------------------- cards */

  const cardById = new Map();

  CHECKS.forEach((check) => {
    const card = document.createElement("article");
    card.className = "check-card";
    card.dataset.state = "idle";
    card.dataset.check = check.id;

    const head = document.createElement("div");
    head.className = "check-head";
    const titles = document.createElement("div");
    const title = document.createElement("div");
    title.className = "check-title";
    title.textContent = check.title;
    const expect = document.createElement("div");
    expect.className = "check-expect";
    expect.textContent = check.expect;
    titles.appendChild(title);
    titles.appendChild(expect);
    head.appendChild(titles);

    const body = document.createElement("div");
    body.className = "check-body";
    const result = document.createElement("span");
    result.className = "check-result";
    result.textContent = "Not run";
    const run = document.createElement("button");
    run.type = "button";
    run.className = "btn small";
    run.textContent = "Run check";
    run.addEventListener("click", () => {
      if (!running) runChecks([check]);
    });
    body.appendChild(result);
    body.appendChild(run);

    card.appendChild(head);
    card.appendChild(body);
    cardsEl.appendChild(card);
    cardById.set(check.id, { card: card, result: result, button: run });
  });

  function setState(check, state, label) {
    const entry = cardById.get(check.id);
    if (!entry) return;
    entry.card.dataset.state = state;
    entry.result.textContent = label;
    entry.button.disabled = running;
  }

  /* ------------------------------------------------------------------ runner */

  async function runOne(check) {
    setState(check, "running", "Running…");
    cleanup();
    await delay(60);
    const refs = check.setup();
    let ok = false;
    if (check.positive) {
      ok = await waitFor(() => check.verify(refs), check.timeout || 3500);
    } else {
      await delay(check.settle || 1200);
      ok = Boolean(check.verify(refs));
    }
    cleanup();
    setState(check, ok ? "pass" : "fail", ok ? "Pass" : "Fail");
    await delay(140);
    return ok;
  }

  async function runChecks(list) {
    if (running) return;
    running = true;
    runBtn.disabled = true;
    clearBtn.disabled = true;
    summary.hidden = true;

    // Self-test forces deterministic settings in this page only. The engine's
    // real configuration is re-applied by the browser on the next page load.
    if (window.__IntegrityBrowser && typeof window.__IntegrityBrowser.configure === "function") {
      window.__IntegrityBrowser.configure({
        enabled: true,
        clickThrough: true,
        blockHiddenTabs: true,
        sensitivity: "balanced",
      });
    }
    await delay(120);

    const results = [];
    for (let i = 0; i < list.length; i += 1) {
      results.push({ check: list[i], ok: await runOne(list[i]) });
    }
    cleanup();

    const passed = results.filter((item) => item.ok).length;
    const failed = results.length - passed;
    summary.hidden = false;
    summary.className = "notice " + (failed === 0 ? "ok" : "bad");
    summary.textContent =
      failed === 0
        ? `All ${passed} scenario(s) passed.`
        : `${passed} passed, ${failed} failed. Check the cards marked Fail.`;

    running = false;
    runBtn.disabled = false;
    clearBtn.disabled = false;
    updateStats();
  }

  runBtn.addEventListener("click", () => runChecks(CHECKS));
  clearBtn.addEventListener("click", () => {
    if (running) return;
    cleanup();
    CHECKS.forEach((check) => setState(check, "idle", "Not run"));
    summary.hidden = true;
  });

  /* ------------------------------------------------------------------ stats */

  function setText(id, value) {
    const node = document.getElementById(id);
    if (node) node.textContent = String(value);
  }

  function updateStats() {
    const engine = window.__IntegrityBrowser;
    if (!engine) {
      banner.className = "harness-banner missing";
      bannerText.textContent =
        "Engine not loaded. Open this page through Integrity Browser, or keep engine.js next to this folder.";
      setText("h-mode", "—");
      return;
    }
    const stats = engine.stats();
    banner.className = "harness-banner ready";
    bannerText.textContent =
      "Engine ready · protection " +
      (stats.enabled ? "on" : "off") +
      " · click-through " +
      (stats.clickThrough ? "on" : "off") +
      " · hidden tabs " +
      (stats.blockHiddenTabs ? "on" : "off") +
      " · " +
      stats.sensitivity;
    setText("h-scanned", I.number(stats.scanned));
    setText("h-suspicious", I.number(stats.suspicious));
    setText("h-neutralized", I.number(stats.neutralized));
    setText("h-through", I.number(stats.clickThroughs));
    setText("h-hidden", I.number(stats.hiddenTabsBlocked));
    setText("h-mode", stats.enabled ? stats.sensitivity : "off");
  }

  setInterval(updateStats, 400);
  updateStats();

  // Expose a tiny hook for scripted checks and future automation.
  window.__integrityHarness = {
    checks: CHECKS,
    run: () => runChecks(CHECKS),
    cleanup: cleanup,
  };
})();
