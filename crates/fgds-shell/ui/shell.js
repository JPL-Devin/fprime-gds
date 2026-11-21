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
};

async function start() {
  const gds = { version: 1, api, records, dictionary: await api.get("/dictionary") };
  const nav = document.getElementById("tabs");
  const main = document.getElementById("tab");
  const tabs = await (await fetch("/ui/tabs")).json();
  for (const tab of tabs) {
    const module = (await import(tab.module)).default;
    if (module.setup) module.setup(gds);
    const button = document.createElement("button");
    button.textContent = module.title || tab.title;
    button.onclick = () => {
      for (const other of nav.children) other.setAttribute("aria-selected", other === button);
      const element = document.createElement(module.element);
      element.gds = gds;
      main.replaceChildren(element);
    };
    nav.append(button);
  }
  if (nav.firstChild) nav.firstChild.click();
  const status = await api.get("/status");
  document.getElementById("link").textContent = "link: " + status.link.state;
}

start();
