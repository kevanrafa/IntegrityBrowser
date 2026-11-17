(function () {
  "use strict";

  const I = window.IBInternal;
  const search = document.getElementById("newtab-search");
  const go = document.getElementById("newtab-go");
  const toggles = {
    "toggle-integrity": "integrity",
    "toggle-click-through": "click_through",
    "toggle-adblock": "adblock",
    "toggle-trackers": "trackers",
  };
  let state = null;
  let suppress = false;

  function navigate() {
    const value = search.value.trim();
    if (!value) {
      search.focus();
      return;
    }
    I.send({ type: "navigate", input: value });
  }

  go.addEventListener("click", navigate);
  search.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      navigate();
    }
  });

  document.addEventListener("keydown", (event) => {
    if (event.key === "/" && document.activeElement !== search) {
      event.preventDefault();
      search.focus();
    }
  });

  Object.keys(toggles).forEach((id) => {
    const input = document.getElementById(id);
    if (!input) return;
    input.addEventListener("change", () => {
      if (suppress) return;
      I.send({ type: "toggle", field: toggles[id], enabled: input.checked });
    });
  });

  document.getElementById("open-settings").addEventListener("click", () => {
    I.send({ type: "open_settings", section: "general" });
  });

  document.getElementById("open-test").addEventListener("click", () => {
    I.send({ type: "open_test_page" });
  });

  function apply() {
    if (!state) return;
    const integrity = state.integrity || {};
    const privacy = state.privacy || {};
    const stats = privacy.stats || state;

    const values = {
      integrity: integrity.masterEnabled,
      click_through: integrity.clickThrough,
      adblock: privacy.adblockEnabled,
      trackers: privacy.trackerBlockingEnabled,
    };

    suppress = true;
    Object.keys(toggles).forEach((id) => {
      const input = document.getElementById(id);
      if (input && typeof values[toggles[id]] === "boolean") {
        input.checked = values[toggles[id]];
      }
    });
    suppress = false;

    I.setText("stat-blocked", I.number(stats.blocked));
    I.setText("stat-ads", I.number(stats.ads));
    I.setText("stat-trackers", I.number(stats.trackers));
    I.setText("stat-inspected", I.number(stats.inspected));

    const proxy = document.getElementById("proxy-chip");
    if (proxy) {
      const on = stats.enabled !== false;
      proxy.textContent = on ? "Local filter proxy active" : "Filtering paused";
      proxy.className = "chip " + (on ? "on" : "warn");
    }
  }

  window.__newtabUpdate = function (next) {
    state = Object.assign({}, state || {}, next || {});
    // The partial update has flat stats; translate them for apply().
    if (!state.privacy && (next.blocked !== undefined || next.inspected !== undefined)) {
      state.privacy = {
        adblockEnabled: next.adblockEnabled,
        trackerBlockingEnabled: next.trackerBlockingEnabled,
        stats: {
          blocked: next.blocked,
          ads: next.ads,
          trackers: next.trackers,
          inspected: next.inspected,
          enabled: next.adblockEnabled || next.trackerBlockingEnabled,
        },
      };
      state.integrity = {
        masterEnabled: next.integrityEnabled,
        clickThrough: next.clickThrough,
        enabled: next.integrityEnabled && !next.siteAllowed,
      };
    }
    apply();
  };

  window.__ibPageUpdate = function (next) {
    state = Object.assign({}, state || {}, next || {});
    apply();
  };

  I.send({ type: "newtab_state_request" });
  search.focus();
})();
