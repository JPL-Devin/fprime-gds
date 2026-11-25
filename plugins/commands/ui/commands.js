// The Commanding tab: a command chosen from the dictionary, one input per argument by
// its type (enum, array and struct included), send, and the history of commands sent.
import { LitElement, html, nothing } from "/ui/lit.js";

const KIND = "fprime.command";

// The value an input of `type` holds when the page opens: an enum its default constant, a boolean nothing
// chosen, a number nothing (null, which the page tells from a text typed and removed, ""). `cleared` is
// what `Clear Arguments` leaves.
function initial(type, cleared = false) {
  switch (type.kind) {
    case "enum": return type.constants.some((c) => c.name === type.default) && !cleared ? type.default : type.constants[0].name;
    case "bool": return null;
    case "array": return Array.from({ length: type.size }, () => initial(type.element, cleared));
    case "struct":
      return Object.fromEntries(type.members.map((m) => [
        m.name, m.count === null ? initial(m.type, cleared) : Array.from({ length: m.count }, () => initial(m.type, cleared)),
      ]));
    case "string": return "";
    default: return null;
  }
}

// The input state for a value taken from a command record (resend).
function fromRecord(type, value) {
  if (value === null || value === undefined) return initial(type);
  switch (type.kind) {
    case "array": return Array.from({ length: type.size }, (_, i) => fromRecord(type.element, Array.isArray(value) ? value[i] : null));
    case "struct":
      return Object.fromEntries(type.members.map((m) => {
        const v = value[m.name];
        return [m.name, m.count === null ? fromRecord(m.type, v)
          : Array.from({ length: m.count }, (_, i) => fromRecord(m.type, (v || [])[i]))];
      }));
    // A text of the Command String stays as typed (`TRUE`, `yes`, `1`), as in the input of the Python page.
    case "bool": return String(value);
    default: return value === FILL_NEEDED && type.kind !== "string" ? null : String(value);
  }
}

// The `Command String` of the Python GDS page (command-string.js): the command of the form as one line,
// `NAME, ARG1, ARG2`, a string in double quotes, an array in brackets, a struct in braces, and
// `<FILL-VALUE>` where nothing is typed yet. A line typed there fills the form; it does not send.
const FILL_NEEDED = "<FILL-VALUE>";
const COMMAND_FORMAT_SPEC = "FULL_COMMAND_NAME[[[, ARG1], ARG2], ...] where ARGN is a decimal number, quoted string, or an enumerated constant";

function lineText(type, state) {
  switch (type.kind) {
    case "array": return `[${state.map((item) => lineText(type.element, item)).join(", ")}]`;
    case "struct":
      return `{${type.members.map((m) => `${m.name}: ${m.count === null ? lineText(m.type, state[m.name])
        : `[${state[m.name].map((item) => lineText(m.type, item)).join(", ")}]`}`).join(", ")}}`;
    case "string": return `"${String(state ?? "").replace(/"/g, '\\"')}"`;
  }
  if (state === null || state === "") return FILL_NEEDED;
  try {
    if (type.kind === "float") return String(parseFloat(state));
    if (type.kind === "bool") {
      const text = String(state).toLowerCase();
      return ["true", "yes"].includes(text) ? "true" : ["false", "no"].includes(text) ? "false" : String(state);
    }
    if (type.kind !== "integer") return String(state);
    if (type.bits === 64) return String(BigInt(state));
    if (type.signed) return String(parseInt(state, 10));
    const radix = { "0x": 16, "0b": 2, "0o": 8 }[state.slice(0, 2)];
    return String(radix === 16 ? parseInt(state, 16) : radix ? parseInt(state.slice(2), radix) : parseInt(state, 10));
  } catch (e) { return FILL_NEEDED; }
}

// The tokens of a line: its name and its arguments. A number stays the text that was typed, so an integer
// of 64 bits is never rounded; an array is a list, a struct an object of its members.
function lineTokens(text) {
  let at = 0;
  const fail = () => { throw new SyntaxError("not a command string"); };
  const space = () => { while (/\s/.test(text[at] ?? "")) at += 1; };
  const value = () => {
    space();
    if (text[at] === '"') {
      let out = "";
      for (at += 1; text[at] !== '"'; at += 1) {
        if (at >= text.length) fail();
        if (text[at] === "\\" && text[at + 1] === '"') at += 1;
        out += text[at];
      }
      at += 1;
      return out;
    }
    if (text[at] === "[" || text[at] === "{") {
      const [close, list] = text[at] === "[" ? ["]", []] : ["}", {}];
      at += 1;
      space();
      if (text[at] === close) { at += 1; return list; }
      for (;;) {
        if (close === "]") list.push(value());
        else {
          const name = value();
          space();
          if (typeof name !== "string" || text[at] !== ":") fail();
          at += 1;
          list[name] = value();
        }
        space();
        if (text[at] === close) { at += 1; return list; }
        if (text[at] !== ",") fail();
        at += 1;
      }
    }
    const from = at;
    while (at < text.length && !',]}:"[{'.includes(text[at])) at += 1;
    const bare = text.slice(from, at).trim();
    if (bare === "" || /\s/.test(bare)) fail();
    return bare;
  };
  const tokens = [];
  for (;;) {
    tokens.push(value());
    space();
    if (at >= text.length) break;
    if (text[at] !== ",") fail();
    at += 1;
  }
  if (typeof tokens[0] !== "string") fail();
  return tokens;
}

// An argument of a command record as the history of the Python GDS page writes it: an array in
// brackets, a struct in braces with its member names, a string in double quotes.
function shown(type, value) {
  if (value === null || value === undefined) return "";
  // A refused command holds its arguments as typed: the page of the Python GDS writes the number read.
  if (typeof value === "string" && type && type.kind === "float" && !Number.isNaN(parseFloat(value))) return String(parseFloat(value));
  if (typeof value === "string" && type && type.kind === "integer") {
    try { return String(BigInt(value)); } catch (e) { return value; }
  }
  switch (type && type.kind) {
    case "array": return `[${value.map((item) => shown(type.element, item)).join(", ")}]`;
    case "struct":
      return `{${type.members.map((m) => `${m.name}: ${m.count === null ? shown(m.type, value[m.name])
        : `[${(value[m.name] || []).map((item) => shown(m.type, item)).join(", ")}]`}`).join(", ")}}`;
    case "string": return `"${String(value).replace(/"/g, '\\"')}"`;
    default: return String(value);
  }
}

// Whether a scalar input holds nothing to send: a number or a boolean with no text. The GDS checks the
// value itself (type, range, constant) and its answer is shown under the form.
function missing(type, state) {
  if (type.kind === "enum" || type.kind === "string") return false;
  return state === null || state === "";
}

// Whether an input of the form holds nothing to send.
function anyMissing(type, state) {
  switch (type.kind) {
    case "array": return state.some((item) => anyMissing(type.element, item));
    case "struct":
      return type.members.some((m) => (m.count === null ? anyMissing(m.type, state[m.name])
        : state[m.name].some((item) => anyMissing(m.type, item))));
    default: return missing(type, state);
  }
}

// Give the state at `holder[key]` the value of a `Command String` or of a history row: a member or element
// that is not given keeps what it holds; a value that is missing, or one value where an array or struct is
// expected, stops the assignment there with a TypeError.
function assign(type, holder, key, given) {
  const each = (element, list, values) => {
    if (values === null || typeof values !== "object") throw new TypeError("not an array or struct");
    list.forEach((_, i) => { if (i in values) assign(element, list, i, values[i]); });
  };
  switch (type.kind) {
    case "array": each(type.element, holder[key], given); return;
    case "struct":
      if (given === null || typeof given !== "object") throw new TypeError("not an array or struct");
      for (const m of type.members) {
        if (!(m.name in given)) continue;
        if (m.count === null) assign(m.type, holder[key], m.name, given[m.name]);
        else each(m.type, holder[key][m.name], given[m.name]);
      }
      return;
    default:
      if (given === null || given === undefined) throw new TypeError("no value");
      holder[key] = fromRecord(type, given);
  }
}

// The JSON value the API takes for an input state.
function convert(type, state) {
  switch (type.kind) {
    case "array": return state.map((item) => convert(type.element, item));
    case "struct":
      return Object.fromEntries(type.members.map((m) => [
        m.name, m.count === null ? convert(m.type, state[m.name]) : state[m.name].map((item) => convert(m.type, item)),
      ]));
    // Numbers and booleans go as typed: the GDS reads the text.
    default: return state;
  }
}

class CommandsTab extends LitElement {
  static properties = {
    filter: { state: true }, selected: { state: true }, values: { state: true }, was: { state: true },
    message: { state: true }, sending: { state: true }, version: { state: true }, historyFilter: { state: true },
    parameters: { state: true }, lineError: { state: true }, unknown: { state: true },
  };

  constructor() {
    super();
    this.filter = "";
    this.historyFilter = "";
    this.parameters = true; // "Show Parameter Commands" of the Python GDS page, ticked at start
    this.lineError = "";
    this.unknown = "";
    this.line = "";
    this.selected = null;
    this.values = {};
    this.forms = new Map(); // by command name: what its form holds, kept while the page is open
    this.was = false; // whether a send was tried: an input that holds nothing is then marked
    this.message = null;
    this.sending = false;
    this.history = [];
    this.version = 0;
  }

  createRenderRoot() { return this; }

  connectedCallback() {
    super.connectedCallback();
    const types = this.gds.types;
    const listed = (this.gds.dictionary.commands || [])
      .map((c) => ({
        name: c.name, opcode: c.opcode, annotation: c.annotation || "",
        params: c.formalParams.map((p) => ({ name: p.name, annotation: p.annotation || "", type: types.resolve(p.type) })),
      }));
    this.commands = listed.slice().sort((a, b) => a.name.localeCompare(b.name));
    // As the Python GDS page (command-input.js): the first command of the dictionary whose mnemonic starts
    // with CMD_NO_OP is selected at start, or else the first command.
    const first = listed.find((c) => c.name.split(".").pop().startsWith("CMD_NO_OP")) || listed[0];
    if (first) this.show(first);
    this.stop = this.gds.records.follow({ kinds: [KIND] }, (records) => {
      this.history.push(...records);
      this.version += 1;
    });
    this.stop.ready.then(() => { this.dataset.history = "loaded"; });
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this.stop) this.stop();
  }

  // The arguments of a history row, in the order and the text of the Python GDS page.
  argumentText(record) {
    const command = this.commands.find((c) => c.name === record.body.name);
    const args = record.body.args || {};
    if (!command || Array.isArray(args)) return this.gds.format.value(Object.values(args));
    return command.params.map((p) => shown(p.type, args[p.name])).join(", ");
  }

  // What the form of `command` holds: the values of its inputs. A command keeps them while another one is shown.
  form(command) {
    if (!this.forms.has(command.name)) {
      this.forms.set(command.name, {
        values: Object.fromEntries(command.params.map((p) => [p.name, initial(p.type)])),
      });
    }
    return this.forms.get(command.name);
  }

  // Show the form of `command`, with what it holds.
  show(command) {
    const form = this.form(command);
    this.selected = command;
    this.values = form.values;
    this.was = false;
    this.lineRefused = false;
    this.unknown = "";
    this.message = null;
    this.cleared = true;
    // The search text does not stay once a command is shown (vue-select of the Python page).
    this.filter = "";
  }

  // A command chosen in the list: its form is shown.
  choose(name) {
    const command = this.commands.find((c) => c.name === name);
    if (command) this.show(command);
  }

  // `Clear Arguments`: every input as at the start but a boolean, which has nothing chosen.
  clear() {
    const command = this.selected;
    for (const p of command.params) this.values[p.name] = initial(p.type, true);
    this.was = false;
    this.lineRefused = false;
    this.message = null;
    this.cleared = true;
    this.requestUpdate();
  }

  // Give the form of `command` the values of `given`, one per argument in order, then show it.
  // Returns false when a value is missing: the values before it are kept and the command shown does not change.
  load(command, given) {
    const form = this.form(command);
    try {
      command.params.forEach((p, i) => assign(p.type, form.values, p.name, i < given.length ? given[i] : null));
    } catch (e) {
      if (!(e instanceof TypeError)) throw e;
      this.requestUpdate();
      return false;
    }
    this.show(command);
    return true;
  }

  // The line is read once after it was typed, also when it is the text the input held before (the browser
  // gives no `change` then).
  entered(input) {
    if (!this.lineTyped) return;
    this.lineTyped = false;
    this.fromLine(input.value, input);
  }

  // A line typed as `Command String`: the form is filled and its fields are checked, as the Python page.
  fromLine(text, input) {
    this.lineError = "";
    // A line that is not taken: the send button sends nothing until the form is used again.
    this.lineRefused = true;
    let tokens;
    try { tokens = lineTokens(text); } catch (e) {
      this.lineError = `Expected command string of the form: ${COMMAND_FORMAT_SPEC}.`;
      // The input shows the line of the form again; an input left empty stays empty.
      if (text.trim() !== "") input.value = this.line;
      return;
    }
    const wanted = tokens[0].toLowerCase();
    const found = this.commands.filter((c) => c.name.toLowerCase() === wanted);
    const command = this.commands.find((c) => c.name === tokens[0]) || (found.length === 1 ? found[0] : null);
    if (!command) {
      // As the Python page: the name is shown as an invalid command, with no argument.
      this.selected = null;
      this.unknown = tokens[0];
      this.message = null;
      this.updateComplete.then(() => { input.value = this.line; });
      return;
    }
    // More arguments than the command takes: refused as on the command line, the line left as typed.
    if (tokens.length - 1 > command.params.length) {
      this.lineError = `${command.name} takes ${command.params.length} arguments, ${tokens.length - 1} given`;
      return;
    }
    // A value the line gives goes to the form of its command; an element or member it does not give keeps
    // what that form holds. A line that leaves an argument out is taken only for the command shown: its
    // values go to the form, the input shows the line of the form (a name alone stays as typed, as on the
    // Python page), and the send button sends what the form shows or marks the fields it cannot send. For another command such a line changes nothing, not the
    // form of that command either, and nothing can be sent.
    // A whole line typed stays when it changes neither the command shown nor a value, as on the Python page.
    const shown = this.selected;
    const form = this.form(command);
    const held = JSON.stringify(form.values);
    const taken = this.load(command, tokens.slice(1));
    if (!taken && shown !== command) {
      form.values = JSON.parse(held);
      return;
    }
    this.lineRefused = false;
    if ((!taken && tokens.length > 1) || this.selected !== shown || JSON.stringify(form.values) !== held) {
      this.updateComplete.then(() => { input.value = this.line; });
    }
  }

  // A history row put back in the form.
  loadRecord(record) {
    const command = this.commands.find((c) => c.name === record.body.name);
    if (!command) return;
    const args = record.body.args || {};
    this.load(command, Array.isArray(args) ? args : command.params.map((p) => args[p.name]));
  }

  // Replace the value at `path` (argument name, then member names and indexes).
  setAt(path, value) {
    let at = this.values;
    for (const key of path.slice(0, -1)) at = at[key];
    at[path[path.length - 1]] = value;
    this.requestUpdate();
  }

  // The request for the form, or null when an input holds nothing to send.
  request() {
    this.was = true;
    if (this.selected.params.some((p) => anyMissing(p.type, this.values[p.name]))) return null;
    return {
      name: this.selected.name,
      args: Object.fromEntries(this.selected.params.map((p) => [p.name, convert(p.type, this.values[p.name])])),
    };
  }

  // The send button pressed: the command shown then. The press takes the focus from the `Command String`
  // input, which reads a line typed there before the click arrives.
  pressed() {
    this.shownAtPress = this.selected;
    this.pressedSend = true;
    addEventListener("mouseup", () => setTimeout(() => { this.pressedSend = false; }), { once: true, capture: true });
  }

  // The click of the send button. A click that itself made the page read a line of another command sends
  // nothing: the form of that command is shown, and the next click sends it.
  submitted() {
    const changed = this.pressedSend && this.shownAtPress !== this.selected;
    this.pressedSend = false;
    if (!changed || this.lineRefused) this.send();
  }

  async send(request = this.selected ? this.request() : null) {
    if (!this.selected && !request) return;
    if (this.lineRefused) {
      this.message = { error: true, text: "Nothing was sent: the Command String is not a command with its arguments" };
      return;
    }
    if (!request) {
      // The inputs that hold nothing are marked.
      this.message = { error: true, text: `${this.selected.name} was not sent: an argument has no value` };
      return;
    }
    this.message = null;
    this.sending = true;
    try {
      const sent = await this.gds.api.post("/commands", request);
      this.message = { error: false, text: `${request.name} sent (record ${sent.seq})` };
      if (this.selected && this.selected.name === request.name) this.was = false;
    } catch (error) {
      this.message = { error: true, text: `${request.name} was not sent: ${error.message}` };
    } finally {
      this.sending = false;
    }
  }

  input(type, state, path, label, description = "") {
    const key = label;
    const scalar = type.kind !== "array" && type.kind !== "struct";
    // Marked once a send was tried: an input that holds nothing.
    const problem = scalar && this.was && missing(type, state);
    const set = (e) => { this.lineRefused = false; this.setAt(path, e.target.value); };
    // The name, with the index or the member, then the description.
    const row = (field) => html`<div class="arg-row top"><label class="arg-name">${`${key}: ${description}`.trim()}</label>${field}</div>`;
    switch (type.kind) {
      case "enum":
        return row(html`<select data-arg=${key} class=${problem ? "invalid" : ""} @change=${set}>
          ${state !== undefined && state !== "" && !type.constants.some((c) => c.name === state)
            // A text of the Command String that is no constant stays shown, as in the input of the Python page.
            ? html`<option value=${state} .selected=${true}>${state}</option>` : nothing}
          ${type.constants.map((c) => html`<option value=${c.name} .selected=${c.name === state}>${c.name}</option>`)}
        </select>`);
      case "bool":
        return row(html`<select data-arg=${key} class=${problem ? "invalid" : ""} @change=${set}>
          ${state === "true" || state === "false" ? nothing
            // Nothing chosen (after `Clear Arguments`), or a text of the Command String that is neither.
            : html`<option value=${state ?? ""} .selected=${true}>${state ?? ""}</option>`}
          <option value="true" .selected=${state === "true"}>True</option>
          <option value="false" .selected=${state === "false"}>False</option>
        </select>`);
      case "array":
        // As the Python page: the name with the type, then each element under the name with its index.
        return html`<fieldset class="arg-group" data-arg-group=${key}>
          <legend>${key} (${type.name})</legend>
          ${state.map((item, i) => this.input(type.element, item, [...path, i], `${key}[${i}]`, description ? `${description}[${i}]` : ""))}
        </fieldset>`;
      case "struct":
        return html`<fieldset class="arg-group" data-arg-group=${key}>
          <legend>${key} (${type.name})</legend>
          ${description ? html`<div><span class="arg-description">Description:</span> ${description}</div>` : nothing}
          ${type.members.map((m) => (m.count === null
            ? this.input(m.type, state[m.name], [...path, m.name], `${key}.${m.name}`, m.annotation || description)
            // A member that is several values: an array.
            : this.input({ kind: "array", name: `${m.type.name}[${m.count}]`, element: m.type }, state[m.name],
              [...path, m.name], `${key}.${m.name}`, m.annotation || description)))}
        </fieldset>`;
      default:
        if (type.kind === "float") {
          // A number input, as in the Python GDS page. While the text is not yet a number ("1e", "-") the
          // input reports an empty value: `updated` then leaves the field alone, so the typing goes on.
          return row(html`<input type="number" step="any" data-arg=${key} class=${problem ? "invalid" : ""}
            .wanted=${state ?? ""} placeholder=${key} @input=${set}>`);
        }
        return row(html`<input type="text" data-arg=${key}
          class=${problem ? "invalid" : ""} .value=${state ?? ""} placeholder=${key} @input=${set}>`);
    }
  }

  updated() {
    for (const input of this.querySelectorAll('input[type="number"]')) {
      const typing = !this.cleared && input.validity.badInput && input.wanted === "";
      if (!typing && input.value !== input.wanted) input.value = input.wanted;
    }
    this.cleared = false;
  }

  render() {
    const format = this.gds.format;
    const needle = this.filter.toLowerCase();
    const command = this.selected;
    // As the Python page: without the box, the commands that set and save parameters are left out, but for
    // the one that is selected.
    const parameter = (c) => /_PRM_(SET|SAVE)$/.test(c.name) && !(command && c.name === command.name);
    const listed = this.commands.filter((c) => (this.parameters || !parameter(c))
     
      // The search of the Python page (vue-select) looks for the text in the name, in any case.
      && (!needle || c.name.toLowerCase().includes(needle)));
    const line = command ? [command.name, ...command.params.map((p) => lineText(p.type, this.values[p.name]))].join(", ") : this.unknown;
    this.line = line;
    const history = this.history
      .filter((r) => format.matches([format.groundTime(r.ground_time),
        [r.body.name, this.argumentText(r)].filter((part) => part !== "").join(", ")], this.historyFilter))
      .slice().reverse();
    return html`
      <div class="commanding">
        <div class="command-list">
          <label for="command-filter">Mnemonic</label>
          <input type="search" id="command-filter" placeholder="Filter commands" .value=${this.filter}
            @input=${(e) => { this.filter = e.target.value; }}>
          <label><input type="checkbox" id="show-parameter-commands" .checked=${this.parameters}
            @change=${(e) => { this.parameters = e.target.checked; }}>Show Parameter Commands</label>
          <select id="command-select" size="14" @change=${(e) => this.choose(e.target.value)}>
            ${listed.map((c) => html`<option value=${c.name} .selected=${!!command && c.name === command.name}
              title=${c.annotation}>${c.name}</option>`)}
          </select>
          ${listed.length ? nothing : html`<div class="muted" id="command-none">Sorry, no matching options.</div>`}
        </div>
        <form class="command-form" id="command-form" novalidate @submit=${(e) => { e.preventDefault(); this.submitted(); }}>
          ${command ? html`
            <h2>Sending Command: <span id="command-name">${command.name}</span></h2>
            <div class="muted"><label>Description:</label> ${command.annotation}</div>
            ${command.params.length ? html`<div><label>Arguments</label></div>` : nothing}
            ${command.params.map((p) => this.input(p.type, this.values[p.name], [p.name], p.name, p.annotation))}
            <div class="toolbar">
              <button class="action" id="command-send" type="submit" ?disabled=${this.sending} @mousedown=${() => this.pressed()}>Send Command</button>
              <button class="action secondary" type="button" id="command-clear" @click=${() => this.clear()}>Clear Arguments</button>
            </div>` : this.unknown ? html`<h2>Sending Command: <span id="command-name">${this.unknown}</span></h2>
              <div class="error-text" id="command-unknown">${this.unknown} is not a command.</div>
              <div class="toolbar"><button class="action" id="command-send" type="submit" @mousedown=${() => this.pressed()}>Send Command</button></div>`
              : html`<div class="muted">Choose a command.</div>`}
          ${this.message ? html`<div id="command-message" class=${this.message.error ? "error-text" : ""}>${this.message.text}</div>` : nothing}
        </form>
      </div>
      <h3>Command String</h3>
      <input type="text" id="command-string" class=${this.lineError ? "invalid" : ""} .value=${line}
        placeholder="Command string of the form: ${COMMAND_FORMAT_SPEC}"
        @focus=${() => { this.lineError = ""; }} @input=${() => { this.lineTyped = true; }} @change=${(e) => this.entered(e.target)}
        @keydown=${(e) => { if (e.key === "Enter") this.entered(e.target); }} @blur=${(e) => this.entered(e.target)}>
      ${this.lineError ? html`<div class="error-text" id="command-string-error">${this.lineError}</div>` : nothing}
      <h3>Command history</h3>
      <div class="toolbar">
        <label>Filters: <input type="search" id="history-filter" .value=${this.historyFilter}
          @input=${(e) => { this.historyFilter = e.target.value; }}></label>
        <span class="muted">${history.length} of ${this.history.length} commands; a row can be loaded into the form or sent again.</span>
      </div>
      <table class="gds" id="command-history">
        <thead><tr><th>Ground time</th><th>Opcode</th><th>Command String</th><th>Outcome</th><th></th></tr></thead>
        <tbody>
          ${history.map((r) => html`<tr class="outcome-${r.body.outcome}" data-seq=${r.seq}>
            <td class="nowrap mono" data-col="ground">${format.groundTime(r.ground_time)}</td>
            <td class="nowrap mono" data-col="opcode">${format.hexId(r.id ?? (this.commands.find((c) => c.name === r.body.name) || {}).opcode)}</td>
            <td class="text" data-col="string">${[r.body.name, this.argumentText(r)].filter((part) => part !== "").join(", ")}</td>
            <td data-col="outcome">${r.body.outcome}${r.body.reason ? `: ${r.body.reason}` : ""}</td>
            <td class="nowrap">
              <button class="action secondary" type="button" data-action="load"
                @click=${() => this.loadRecord(r)}>Load</button>
              <button class="action secondary" type="button" data-action="resend"
                @click=${() => this.send({ name: r.body.name, args: r.body.args || {} })}>Resend</button>
            </td>
          </tr>`)}
        </tbody>
      </table>`;
  }
}
customElements.define("commands-tab", CommandsTab);

export default { id: "commands", title: "Commanding", element: "commands-tab" };
