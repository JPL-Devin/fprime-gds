// The page shell: navigation, status bar, and the `gds` object every tab receives.
// Tabs reach the GDS only through `gds`, which uses the public API (/api/v1) alone.
import { html, render } from "/ui/lit.js";

const api = {
  async get(path) { return request(path); },
  async post(path, body) {
    return request(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  },
};

async function request(path, options) {
  const response = await fetch("/api/v1" + path, options);
  let body = null;
  try { body = await response.json(); } catch { /* an answer without JSON */ }
  if (!response.ok) {
    const error = new Error(body && body.error ? body.error.message : response.statusText);
    error.code = body && body.error ? body.error.code : String(response.status);
    throw error;
  }
  return body;
}

function params(query = {}) {
  const out = new URLSearchParams();
  for (const kind of query.kinds || []) out.append("kind", kind);
  if (query.afterSeq !== undefined) out.set("after_seq", query.afterSeq);
  if (query.beforeSeq !== undefined) out.set("before_seq", query.beforeSeq);
  if (query.run !== undefined) out.set("run", query.run);
  if (query.limit !== undefined) out.set("limit", query.limit);
  if (query.text) out.set("text", query.text);
  for (const filter of query.filters || []) out.append("filter", filter);
  return out.toString();
}

const PAGE = 5000;

const records = {
  // Stored records after `query.afterSeq`, then live ones, on one WebSocket. Without
  // `afterSeq`, live only. Returns the function that stops the subscription.
  subscribe(query, callback) {
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    const socket = new WebSocket(`${scheme}://${location.host}/api/v1/stream?${params(query)}`);
    socket.onmessage = (message) => {
      const data = JSON.parse(message.data);
      if (data.type === "record") callback(data.record);
    };
    return () => socket.close();
  },
  query(query) { return api.get("/records?" + params(query)); },
  latest(kind) { return api.get("/records/latest?kind=" + encodeURIComponent(kind)); },

  // History then live, each record once and in order: the stored records are read page
  // by page through the record API, then the stream is opened after the last one read, so
  // the server replays whatever arrived in between. A dropped or lagging stream is
  // reopened after the last record delivered. `callback` receives arrays of records.
  follow(query, callback) {
    let last = BigInt(query.afterSeq ?? 0);
    let stopped = false;
    let socket = null;
    let timer = null;
    const deliver = (list) => {
      const fresh = list.filter((r) => BigInt(r.seq) > last);
      if (!fresh.length || stopped) return;
      last = BigInt(fresh[fresh.length - 1].seq);
      callback(fresh);
    };
    const open = () => {
      if (stopped) return;
      const scheme = location.protocol === "https:" ? "wss" : "ws";
      const q = params({ kinds: query.kinds, afterSeq: last.toString() });
      socket = new WebSocket(`${scheme}://${location.host}/api/v1/stream?${q}`);
      socket.onmessage = (message) => {
        const data = JSON.parse(message.data);
        if (data.type === "record") deliver([data.record]);
      };
      socket.onclose = () => { if (!stopped) timer = setTimeout(open, 1000); };
    };
    const history = async () => {
      for (;;) {
        if (stopped) return;
        const page = await records.query({ kinds: query.kinds, afterSeq: last.toString(), limit: PAGE });
        deliver(page.records);
        if (page.next === null || page.next === undefined) break;
        // A page may end on records this query does not deliver; never move backwards.
        if (BigInt(page.next) > last) last = BigInt(page.next);
      }
      open();
    };
    const done = history().catch((error) => { console.error("history", error); open(); });
    const stop = () => {
      stopped = true;
      clearTimeout(timer);
      if (socket) socket.close();
    };
    stop.ready = done; // resolves when the history has been delivered
    return stop;
  },
};

// ---- formatting -----------------------------------------------------------------------

const pad = (n, width) => String(n).padStart(width, "0");

const format = {
  // Flight time as the Python GDS shows it: a calendar time when the time base is the
  // workstation clock (2), `seconds.microseconds` otherwise. Microseconds are kept.
  flightTime(time) {
    if (!time) return "";
    if (time.base === 2) {
      const date = new Date(time.seconds * 1000).toISOString();
      return `${date.slice(0, 19)}.${pad(time.microseconds, 6)}Z`;
    }
    return `${time.seconds}.${pad(time.microseconds, 6)}`;
  },
  groundTime(iso) { return iso || ""; },
  hexId(id) { return id === null || id === undefined ? "" : "0x" + BigInt(id).toString(16); },
  // A decoded value as one line of text: arrays as [a, b], structures as {name: value}.
  value(value) {
    if (value === null || value === undefined) return "";
    if (Array.isArray(value)) return "[" + value.map(format.value).join(", ") + "]";
    if (typeof value === "object") {
      return "{" + Object.entries(value).map(([k, v]) => `${k}: ${format.value(v)}`).join(", ") + "}";
    }
    return String(value);
  },
  component(name) {
    const at = String(name).lastIndexOf(".");
    return at < 0 ? "" : name.slice(0, at);
  },
};

// ---- dictionary types -----------------------------------------------------------------

// The types of the dictionary, resolved for display and for input: aliases and qualified
// names are followed to integer, float, bool, string, enum, array or struct.
function typeResolver(dictionary) {
  const named = new Map((dictionary.typeDefinitions || []).map((t) => [t.qualifiedName, t]));
  const resolve = (type, depth = 0) => {
    if (!type || depth > 32) return { kind: "unknown", name: type ? type.name : "?" };
    switch (type.kind) {
      case "integer": return { kind: "integer", name: type.name, bits: type.size, signed: type.signed };
      case "float": return { kind: "float", name: type.name, bits: type.size };
      case "bool": return { kind: "bool", name: "bool" };
      case "string": return { kind: "string", name: "string", max: type.size };
      case "qualifiedIdentifier": {
        const found = named.get(type.name);
        return found ? resolve(found, depth + 1) : { kind: "unknown", name: type.name };
      }
      case "alias": return { ...resolve(type.underlyingType || type.type, depth + 1), alias: type.qualifiedName };
      case "enum":
        return {
          kind: "enum", name: type.qualifiedName,
          constants: type.enumeratedConstants.map((c) => ({ name: c.name, value: c.value })),
          default: String(type.default || "").split(".").pop(),
        };
      case "array":
        return {
          kind: "array", name: type.qualifiedName, size: type.size,
          element: resolve(type.elementType, depth + 1), default: type.default,
        };
      case "struct":
        return {
          kind: "struct", name: type.qualifiedName, default: type.default,
          members: Object.entries(type.members)
            .sort((a, b) => a[1].index - b[1].index)
            .map(([name, m]) => ({ name, type: resolve(m.type, depth + 1), count: m.size ?? null })),
        };
      default: return { kind: "unknown", name: type.name || type.kind };
    }
  };
  return { resolve, named };
}

// ---- status ---------------------------------------------------------------------------

const status = {
  current: null,
  error: null,
  listeners: new Set(),
  subscribe(callback) {
    this.listeners.add(callback);
    if (this.current) callback(this.current);
    return () => this.listeners.delete(callback);
  },
  async refresh() {
    try {
      this.current = await api.get("/status");
      this.error = null;
    } catch (error) {
      this.error = error;
    }
    renderStatus();
    if (this.current && !this.error) for (const listener of this.listeners) listener(this.current);
  },
};

function renderStatus() {
  const s = status.current;
  const counters = (s && s.counters) || {};
  const link = status.error ? "unreachable" : s ? s.link.state : "…";
  const badge = (text, good) => html`<span class="badge ${good ? "good" : "bad"}">${text}</span>`;
  const item = (label, value) => html`<span><span class="label">${label}</span> ${value}</span>`;
  const errors = (counters["kind.gds.decode_error"] || 0) + (counters["kind.gds.unknown_packet"] || 0)
    + (counters["kind.raw.discarded"] || 0) + (counters["kind.gds.plugin_fault"] || 0);
  render(html`
    ${item("GDS", badge(status.error ? "unreachable" : "running", !status.error))}
    ${item("Link", html`<span id="link-state">${badge(link, link === "connected")}</span> ${s && s.link.peer ? s.link.peer : ""}`)}
    ${item("Storage", s ? badge(s.storage.state, s.storage.state === "ok") : "")}
    ${item("Run", s ? s.run : "")}
    ${item("Records", s ? s.cursor : "")}
    ${item("Received", `${counters.bytes_received || 0} B / ${counters.packets_received || 0} packets`)}
    ${item("Decode problems", errors ? badge(errors, false) : "0")}
    ${item("Dictionary", gds.version ? gds.version.dictionary : "")}
  `, document.querySelector("#status"));
}

// ---- settings (kept in the browser) ---------------------------------------------------

const settings = {
  get(name, fallback) {
    try {
      const stored = localStorage.getItem("gds." + name);
      return stored === null ? fallback : JSON.parse(stored);
    } catch { return fallback; }
  },
  set(name, value) {
    try { localStorage.setItem("gds." + name, JSON.stringify(value)); } catch { /* private mode */ }
  },
};

export const gds = { api, records, format, status, settings, dictionary: null, types: null, version: null, tabs: [] };
window.gds = gds;

// ---- shared elements ------------------------------------------------------------------

// <gds-value>: a decoded value, shown as text whatever it contains.
class GdsValue extends HTMLElement {
  set value(value) { this.textContent = format.value(value); }
}
customElements.define("gds-value", GdsValue);

// ---- tabs -----------------------------------------------------------------------------

const tabs = []; // {id, title, module, button, panel, definition, error}

function errorPanel(tab, error) {
  const panel = document.createElement("div");
  panel.className = "error-panel";
  panel.dataset.plugin = tab.id;
  render(html`
    <h2>The tab “${tab.title}” failed</h2>
    <div>Plugin <b class="plugin">${tab.id}</b> (${tab.module}) could not be shown. The other tabs keep working.</div>
    <pre class="mono">${String((error && (error.stack || error.message)) || error)}</pre>
  `, panel);
  return panel;
}

function fail(tab, error) {
  console.error(`tab ${tab.id}`, error);
  tab.error = error;
  tab.button.classList.add("failed");
  const panel = errorPanel(tab, error);
  panel.hidden = tab.panel ? tab.panel.hidden : true;
  if (tab.panel) tab.panel.replaceWith(panel); else document.querySelector("#tab").append(panel);
  tab.panel = panel;
}

function mount(tab) {
  if (tab.panel) return;
  const main = document.querySelector("#tab");
  try {
    if (tab.error) throw tab.error;
    if (!customElements.get(tab.definition.element)) {
      throw new Error(`custom element <${tab.definition.element}> is not defined by the module`);
    }
    // An exception in the constructor or in connectedCallback is reported to the window,
    // not to createElement() or append().
    let thrown = null;
    const catcher = (event) => { thrown = event.error || new Error(event.message); event.preventDefault(); };
    window.addEventListener("error", catcher);
    try {
      const element = document.createElement(tab.definition.element);
      if (!thrown && !(element instanceof customElements.get(tab.definition.element))) {
        thrown = new Error(`<${tab.definition.element}> could not be created`);
      }
      if (!thrown) {
        element.gds = gds;
        main.append(element);
        tab.panel = element;
      }
    } finally { window.removeEventListener("error", catcher); }
    if (thrown) throw thrown;
  } catch (error) {
    fail(tab, error);
  }
}

function select(id) {
  const tab = tabs.find((t) => t.id === id) || tabs[0];
  if (!tab) return;
  mount(tab);
  for (const other of tabs) {
    other.button.setAttribute("aria-selected", String(other === tab));
    if (other.panel) other.panel.hidden = other !== tab;
  }
  if (location.hash !== "#" + tab.id) history.replaceState(null, "", "#" + tab.id);
  document.title = `${tab.title} - F Prime GDS`;
}

// Errors thrown later by a tab's own code (rendering, callbacks) are traced to the tab by
// the address of its module.
function blame(error) {
  const stack = String((error && error.stack) || "");
  const tab = tabs.find((t) => stack.includes(`/ui/plugins/${t.id}/`));
  if (tab && !tab.error) fail(tab, error);
}
window.addEventListener("error", (event) => blame(event.error));
window.addEventListener("unhandledrejection", (event) => blame(event.reason));

async function start() {
  const nav = document.querySelector("#tabs");
  try {
    [gds.dictionary, gds.version] = await Promise.all([api.get("/dictionary"), api.get("/version")]);
    gds.types = typeResolver(gds.dictionary);
  } catch (error) {
    document.querySelector("#tab").append(errorPanel({ id: "shell", title: "GDS", module: "/api/v1" }, error));
    return;
  }
  status.refresh();
  setInterval(() => status.refresh(), 1000);

  const listed = await (await fetch("/ui/tabs")).json();
  const modules = []; // without a title: loaded for `setup` only (a renderer, an input)
  for (const entry of listed) {
    const tab = { id: entry.id, title: entry.title, module: entry.module, panel: null, error: null, definition: {} };
    if (!entry.title) { modules.push(tab); continue; }
    tab.button = document.createElement("button");
    tab.button.textContent = entry.title;
    tab.button.setAttribute("role", "tab");
    tab.button.dataset.tab = entry.id;
    tab.button.onclick = () => select(entry.id);
    nav.append(tab.button);
    tabs.push(tab);
  }
  await Promise.all(modules.map(async (entry) => {
    try {
      const module = (await import(entry.module)).default || {};
      if (module.setup) await module.setup(gds);
    } catch (error) {
      console.error(`module ${entry.id}`, error);
    }
  }));
  gds.tabs = tabs.map((t) => ({ id: t.id, title: t.title, module: t.module }));
  // One module failing to load or to set up must not stop the others (UI-3).
  await Promise.all(tabs.map(async (tab) => {
    try {
      const module = await import(tab.module);
      tab.definition = module.default || {};
      if (!tab.definition.element) throw new Error("the module has no default export naming its element");
      if (tab.definition.setup) await tab.definition.setup(gds);
    } catch (error) {
      tab.error = error;
      tab.button.classList.add("failed");
    }
  }));
  window.addEventListener("hashchange", () => select(location.hash.slice(1)));
  select(location.hash.slice(1));
  document.body.dataset.ready = "true";
}

start();
