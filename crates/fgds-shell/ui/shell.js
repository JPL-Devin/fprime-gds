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
  // Replay from `afterSeq`, then live. Returns a function that ends the subscription.
  // When the server says `lagged` or the connection closes, the subscription is opened
  // again after the last record delivered, so nothing is missed or repeated.
  subscribe(query, callback) {
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    let last = query.afterSeq;
    let ended = false;
    let socket;
    const open = () => {
      socket = new WebSocket(`${scheme}://${location.host}/api/v1/stream?${params({ ...query, afterSeq: last })}`);
      socket.onmessage = (message) => {
        const data = JSON.parse(message.data);
        if (data.type === "record") {
          last = data.record.seq;
          callback(data.record);
        }
        // The stored records have all arrived; `onLive` is optional.
        if (data.type === "live") {
          if (last === undefined) last = data.seq;
          if (query.onLive) query.onLive(data.seq);
        }
        if (data.type === "lagged") last = data.last_seq;
      };
      socket.onclose = () => { if (!ended) setTimeout(open, 1000); };
    };
    open();
    return () => { ended = true; socket.close(); };
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
        // Records were dropped for this subscriber: read again from the last one delivered.
        if (data.type === "lagged") socket.close();
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
  // The filter of the tables of the Python GDS page (`filter` in its `utils.js`): the text is cut at spaces,
  // every word must be in the column texts joined by spaces, case ignored; `a -or- b` takes either. Quotes
  // are characters like any other.
  matches(cells, text) {
    const all = [];
    let either = false;
    for (const word of text.toLowerCase().split(" ")) {
      if (word === "-or-") either = true;
      else if (either && all.length) { all[all.length - 1].push(word); either = false; }
      else { all.push([word]); either = false; }
    }
    const row = cells.map(String).join(" ").toLowerCase();
    return all.every((words) => words.some((word) => row.includes(word)));
  },
  // Flight time as the Python GDS shows it: a calendar time when the time base is the
  // workstation clock (2), `seconds.microseconds` otherwise. Microseconds are kept.
  // The text the Python GDS page writes: calendar time to the millisecond for the workstation
  // time base, otherwise seconds, a point and the microseconds as a number.
  flightTime(time) {
    if (!time) return "";
    if (time.base === 2) {
      return new Date(time.seconds * 1000 + time.microseconds / 1000).toISOString();
    }
    return `${time.seconds}.${time.microseconds}`;
  },
  // An array or a structure as the Python GDS page writes it: the members as JSON over several
  // lines, which a table cell shows on one line (`[ "0", "2" ]`).
  composite(value) {
    return typeof value === "object" && value !== null ? JSON.stringify(value, null, 2) : format.value(value);
  },
  // A text of the flight software. The Python GDS page escapes `<` and `>` and sets the result as HTML, so
  // `&amp;` reads `&` there: the same is read here from an inert template, nothing runs. That page also puts
  // headings and spans back as elements; here their tags stay visible as the characters that were sent.
  flightText(text) {
    if (typeof text !== "string" || !text.includes("&")) return text;
    const inert = document.createElement("template");
    inert.innerHTML = text.replaceAll("<", "&lt;").replaceAll(">", "&gt;");
    return inert.content.textContent;
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

// Settings of the Advanced tab that last as long as the page, as those of the Python GDS page.
const session = { channels_display_last_received: true };

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

// The columns of a table, as the tables of the Python GDS page: a click on a header sorts by the text of that
// column (ascending, then descending, then the order of arrival again), and each column can be hidden (kept in
// the browser). `titles` is a list of [key, title]; `changed` is called when the table must be drawn again.
function columns(name, titles, changed) {
  const hidden = new Set(settings.get(`${name}.hidden`, settings.get(`${name}.ground`, true) ? [] : ["ground"]));
  const sort = { key: null, ascending: true };
  const shows = (key) => !hidden.has(key);
  const click = (key) => {
    if (sort.key !== key) Object.assign(sort, { key, ascending: true });
    else if (sort.ascending) sort.ascending = false;
    else Object.assign(sort, { key: null, ascending: true });
    changed();
  };
  const toggle = (key, on) => {
    if (on) hidden.delete(key); else hidden.add(key);
    settings.set(`${name}.hidden`, [...hidden]);
    changed();
  };
  return {
    shows,
    sorted: () => sort.key,
    // One box per column.
    chooser: () => html`<span class="columns" id="${name}-columns">Columns ${titles.map(([key, title]) => html`<label>
      <input type="checkbox" id="${name}-${key}" .checked=${shows(key)} @change=${(e) => toggle(key, e.target.checked)}>${title}</label>`)}</span>`,
    head: (first = "") => html`<tr>${first}${titles.map(([key, title]) => html`<th class="sortable" data-col=${key} ?hidden=${!shows(key)}
      @click=${() => click(key)}>${title}${sort.key === key ? html` <span class="sort">${sort.ascending ? "\u25BE" : "\u25B4"}</span>` : ""}</th>`)}</tr>`,
    // `rows` in the order asked for; `text(row, key)` is what the column shows.
    sort: (rows, text) => {
      if (sort.key === null) return rows;
      const keyed = rows.map((row) => ({ row, key: String(text(row, sort.key)) }));
      keyed.sort((a, b) => (sort.ascending ? 1 : -1) * a.key.localeCompare(b.key, undefined, { numeric: true }));
      return keyed.map((k) => k.row);
    },
  };
}

export const gds = { api, records, format, status, settings, session, columns, dictionary: null, types: null, version: null, tabs: [] };
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

// The tab named in the address: as the Python GDS page the fragment is the name the tab shows (`#Channels`);
// the identifier of a tab is taken too. With no fragment the first tab is shown and the address is left as it is.
function fromAddress() {
  let name = location.hash.slice(1);
  try { name = decodeURIComponent(name); } catch { /* shown as the first tab */ }
  const tab = tabs.find((t) => t.title === name) || tabs.find((t) => t.id === name);
  select(tab ? tab.id : "", name !== "");
}

function select(id, address = true) {
  const tab = tabs.find((t) => t.id === id) || tabs[0];
  if (!tab) return;
  mount(tab);
  for (const other of tabs) {
    other.button.setAttribute("aria-selected", String(other === tab));
    if (other.panel) other.panel.hidden = other !== tab;
  }
  const fragment = "#" + encodeURIComponent(tab.title);
  if (address && location.hash !== fragment) history.replaceState(null, "", fragment);
  document.title = `${tab.title} - F Prime GDS`;
}

// Errors thrown later by a tab's own code (rendering, callbacks) are traced to the tab by
// the address of its module.
function blame(error) {
  const stack = String((error && error.stack) || "");
  const tab = tabs.find((t) => stack.includes(`/ui/plugins/${t.id}/`));
  if (tab && !tab.error) fail(tab, error);
}
// The counters of the Python GDS page: the events of these two severities the page received, and the
// errors of the page itself; "99+" from 99 on.
const counts = { WARNING_HI: 0, FATAL: 0, GDS_Errors: 0 };
function showCounts() {
  document.querySelector("#counters").replaceChildren(...Object.entries(counts).map(([name, count]) => {
    const counter = document.createElement("span");
    counter.dataset.counter = name;
    counter.textContent = `${name.replace("_", " ")}: ${count >= 99 ? "99+" : count}`;
    return counter;
  }));
}
function pageError(error) {
  counts.GDS_Errors += 1;
  showCounts();
  blame(error);
}
window.addEventListener("error", (event) => pageError(event.error));
window.addEventListener("unhandledrejection", (event) => pageError(event.reason));

async function start() {
  const nav = document.querySelector("#tabs");
  try {
    [gds.dictionary, gds.version] = await Promise.all([api.get("/dictionary"), api.get("/version")]);
    gds.types = typeResolver(gds.dictionary);
  } catch (error) {
    document.querySelector("#tab").append(errorPanel({ id: "shell", title: "GDS", module: "/api/v1" }, error));
    return;
  }
  showCounts();
  // As the Python GDS page: a button that opens the page in a new window, and an indicator that is on while
  // events or channels arrive and goes off 5 s after the last of them.
  document.querySelector("#new-window").onclick = () => window.open(window.location);
  const flow = document.querySelector("#data-flow");
  let quiet = null;
  gds.records.follow({ kinds: ["fprime.event", "fprime.channel"], afterSeq: (await api.get("/status")).cursor }, (records) => {
    if (!records.length) return;
    flow.dataset.flow = "on";
    flow.title = "Data is arriving";
    clearTimeout(quiet);
    quiet = setTimeout(() => { flow.dataset.flow = "off"; flow.title = "No data for 5 seconds"; }, 5000);
  });
  gds.records.follow({ kinds: ["fprime.event"] }, (records) => {
    for (const record of records) if (record.body.severity in counts) counts[record.body.severity] += 1;
    showCounts();
  });
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
  window.addEventListener("hashchange", fromAddress);
  fromAddress();
  document.body.dataset.ready = "true";
}

start();
