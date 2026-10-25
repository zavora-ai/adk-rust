/* AWP website experiments (spec 3.5 §7.12). Served as /_awp/experiments.js.
 *
 * Assigns this tab to an arm by a stable hash of an anonymous bucket, shows the variant's
 * own static file to the variant arm, and counts exposures and goals through the edge
 * beacon. No cookie is set or read; the bucket stays in this tab's sessionStorage and is
 * never sent. With script off the page shows the control and measures nothing.
 */
(function () {
  "use strict";
  var tag = document.currentScript;
  if (!tag) return;
  var data = tag.dataset;
  var id = data.awpExperiment;
  var arm = data.arm || "";
  var beacon = data.beacon;
  if (!id || !beacon) return;

  var store = null;
  try { store = window.sessionStorage; } catch (e) { store = null; }
  function get(key) { try { return store ? store.getItem(key) : null; } catch (e) { return null; } }
  function set(key, value) { try { if (store) store.setItem(key, value); } catch (e) { /* private mode */ } }

  function send(name) {
    var body = JSON.stringify({ kind: "event", name: name });
    try {
      if (navigator.sendBeacon && navigator.sendBeacon(beacon, body)) return;
    } catch (e) { /* fall through */ }
    try {
      fetch(beacon, { method: "POST", body: body, keepalive: true, mode: "no-cors", credentials: "omit" });
    } catch (e) { /* measurement never breaks the page */ }
  }

  // FNV-1a over UTF-16 code units: the same function as adk_website::experiments::bucket_hash.
  function hash(text) {
    var h = 0x811c9dc5;
    for (var i = 0; i < text.length; i++) {
      h ^= text.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h >>> 0;
  }

  function bucket() {
    var value = get("awp-bucket");
    if (!value) {
      var words = new Uint32Array(2);
      if (window.crypto && window.crypto.getRandomValues) {
        window.crypto.getRandomValues(words);
      } else {
        words[0] = Math.floor(Math.random() * 4294967296);
        words[1] = Math.floor(Math.random() * 4294967296);
      }
      value = ("0000000" + words[0].toString(16)).slice(-8) + ("0000000" + words[1].toString(16)).slice(-8);
      set("awp-bucket", value);
    }
    return value;
  }

  var key = "awp-exp:" + id;
  // A link that names the arm (#awp-exp=headline.b), such as one in a nurture email,
  // attributes the visit to that arm; the fragment never reaches a server.
  var named = /(?:^#|&)awp-exp=([a-z0-9-]+)\.([ab])(?:&|$)/.exec(location.hash);
  if (named && named[1] === id) set(key, named[2]);

  if (arm === "a" || arm === "b") {
    var stored = get(key);
    var share = parseFloat(data.allocation);
    var assigned = stored || (hash(id + ":" + bucket()) / 4294967296 < share ? "b" : "a");
    if (arm === "a" && assigned === "b" && data.variant) {
      set(key, "b");
      location.replace(data.variant + location.search + location.hash);
      return;
    }
    set(key, arm);
    // One exposure per tab session and arm.
    if (get(key + ":exposure") !== arm) {
      set(key + ":exposure", arm);
      send("exp." + id + "." + arm + ".exposure");
    }
  }

  // A goal counts once per tab session and arm, for the arm this tab was shown.
  function goal(name) {
    var current = get(key);
    if (!current || !/^[a-z0-9._-]{1,64}$/.test(name)) return;
    var once = key + ":goal:" + current + ":" + name;
    if (get(once)) return;
    set(once, "1");
    send("exp." + id + "." + current + "." + name);
  }

  // A form marked data-awp-goal counts when it is submitted; a link or button when clicked.
  document.addEventListener("submit", function (event) {
    var form = event.target;
    var name = form && form.getAttribute ? form.getAttribute("data-awp-goal") : null;
    if (name) goal(name);
  }, true);
  document.addEventListener("click", function (event) {
    var target = event.target;
    var marked = target && target.closest
      ? target.closest("a[data-awp-goal], button[data-awp-goal]")
      : null;
    if (marked) goal(marked.getAttribute("data-awp-goal"));
  }, true);

  var api = window.awpExperiments = window.awpExperiments || { handlers: [] };
  api.handlers.push(goal);
  api.goal = function (name) {
    for (var i = 0; i < api.handlers.length; i++) api.handlers[i](name);
  };
})();
