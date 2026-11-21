// The page shell: lists the tabs the plugins registered, loads each tab's ES module,
// and gives every tab the same `gds` object (section 4.8 of the design).

const api = {
  async get(path) { return request(path); },
  async post(path, body) {
    return request(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  },
};

async function request(path, options) {
  const response = await fetch("/api/v1" + path, options);
  const body = await response.json();
  if (!response.ok) throw new Error(body.error ? body.error.message : response.statusText);
  return body;
}

function params(query) {
  const out = new URLSearchParams();
  for (const kind of query.kinds || []) out.append("kind", kind);
  if (query.afterSeq !== undefined) out.set("after_seq", query.afterSeq);
  if (query.limit !== undefined) out.set("limit", query.limit);
  return out.toString();
}

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
};

async function start() {
  const gds = { version: 1, api, records, dictionary: await api.get("/dictionary") };
  const nav = document.getElementById("tabs");
  const main = document.getElementById("tab");
  const tabs = await (await fetch("/ui/tabs")).json();
  for (const tab of tabs) {
    // A tab whose module fails to load or to set up shows the error; the others load.
    let module;
    let failure;
    try {
      module = (await import(tab.module)).default;
      if (module.setup) module.setup(gds);
    } catch (error) {
      failure = error;
      module = {};
    }
    // No title: a module without a tab (a renderer, an argument input).
    const title = module.title || tab.title || (failure && tab.id);
    if (!title) continue;
    const button = document.createElement("button");
    button.textContent = title;
    button.onclick = () => {
      for (const other of nav.children) other.setAttribute("aria-selected", other === button);
      let element;
      try {
        if (failure) throw failure;
        element = document.createElement(module.element);
        element.gds = gds;
      } catch (error) {
        element = document.createElement("pre");
        element.className = "tab-error";
        element.textContent = `Tab "${title}" could not be loaded: ${error.message || error}`;
      }
      main.replaceChildren(element);
    };
    nav.append(button);
  }
  if (nav.firstChild) nav.firstChild.click();
  const status = await api.get("/status");
  document.getElementById("link").textContent = "link: " + status.link.state;
}

start();
