// Runs shell.js against a minimal page: one tab module that fails to load, one whose
// setup throws, one that works; then a subscription whose server says `lagged`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.attributes = {}; this.textContent = ""; }
  append(child) { this.children.push(child); }
  replaceChildren(child) { this.children = [child]; }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  get firstChild() { return this.children[0]; }
  click() { this.onclick(); }
}

const page = { tabs: new Element("nav"), tab: new Element("main"), link: new Element("span") };
const document = { getElementById: (id) => page[id], createElement: (tag) => new Element(tag) };

const answers = {
  "/api/v1/dictionary": {},
  "/api/v1/status": { link: { state: "connected" } },
  "/ui/tabs": [
    { id: "missing", title: "Missing", module: "/missing.js" },
    { id: "broken", title: null, module: "/broken.js" },
    { id: "good", title: "Good", module: "/good.js" },
  ],
};
const fetch = async (path) => ({ ok: true, json: async () => answers[path] });

let gds;
const modules = {
  "/broken.js": { default: { setup() { throw new Error("setup bug"); } } },
  "/good.js": { default: { element: "good-tab", setup(given) { gds = given; } } },
};
const load = async (path) => {
  if (!modules[path]) throw new Error(`cannot load ${path}`);
  return modules[path];
};

const sockets = [];
class WebSocket {
  constructor(url) { this.url = url; sockets.push(this); }
  close() { if (this.onclose) this.onclose(); }
}

const source = readFileSync(process.argv[2], "utf8").replaceAll("await import(", "await load(");
const run = new Function("document", "fetch", "load", "WebSocket", "location", "setTimeout", source);
const timers = [];
run(document, fetch, load, WebSocket, { protocol: "http:", host: "gds" }, (f) => timers.push(f));
await new Promise((resolve) => setImmediate(resolve));
await new Promise((resolve) => setImmediate(resolve));

// Every tab has its button, and the page finished loading.
assert.deepEqual(page.tabs.children.map((b) => b.textContent), ["Missing", "broken", "Good"]);
assert.equal(page.link.textContent, "link: connected");
assert.match(page.tab.children[0].textContent, /Tab "Missing" could not be loaded: cannot load \/missing.js/);
page.tabs.children[1].click();
assert.match(page.tab.children[0].textContent, /setup bug/);
page.tabs.children[2].click();
assert.equal(page.tab.children[0].tag, "good-tab");
assert.equal(page.tab.children[0].gds, gds);

// A subscription continues after the last record delivered when the server ends it.
const seen = [];
const end = gds.records.subscribe({ kinds: ["a.b"], afterSeq: "3" }, (record) => seen.push(record.seq));
assert.equal(sockets[0].url, "ws://gds/api/v1/stream?kind=a.b&after_seq=3");
sockets[0].onmessage({ data: JSON.stringify({ type: "record", record: { seq: "5" } }) });
sockets[0].onmessage({ data: JSON.stringify({ type: "lagged", last_seq: "5" }) });
sockets[0].onclose();
assert.equal(timers.length, 1);
timers.pop()();
assert.equal(sockets[1].url, "ws://gds/api/v1/stream?kind=a.b&after_seq=5");
sockets[1].onmessage({ data: JSON.stringify({ type: "record", record: { seq: "6" } }) });
assert.deepEqual(seen, ["5", "6"]);
// Ended by the tab: no reconnection.
end();
assert.equal(timers.length, 0);

// A live-only subscription that is cut continues after the `live` marker, not from now.
gds.records.subscribe({ kinds: ["a.b"] }, () => {});
sockets[2].onmessage({ data: JSON.stringify({ type: "live", seq: "9" }) });
sockets[2].onclose();
timers.pop()();
assert.equal(sockets[3].url, "ws://gds/api/v1/stream?kind=a.b&after_seq=9");
console.log("  ok: page shell with failing tabs and a stream that ends");
