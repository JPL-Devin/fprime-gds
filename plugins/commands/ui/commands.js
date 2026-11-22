// The Commanding tab: a command chosen from the dictionary, one input per argument by
// its type (enum, array and struct included), send, and the history of commands sent.
import { LitElement, html, nothing } from "/ui/lit.js";

const KIND = "fprime.command";

// The value an input of `type` starts with. Scalars start empty, so nothing is sent by accident: null until
// something is typed, as in the Python GDS page, which tells it from a text typed and removed ("").
function initial(type) {
  switch (type.kind) {
    case "enum": return type.constants.some((c) => c.name === type.default) ? type.default : type.constants[0].name;
    case "bool": return null;
    case "array": return Array.from({ length: type.size }, () => initial(type.element));
    case "struct":
      return Object.fromEntries(type.members.map((m) => [
        m.name, m.count === null ? initial(m.type) : Array.from({ length: m.count }, () => initial(m.type)),
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
    case "bool": {
      const text = String(value).toLowerCase();
      return text === "yes" ? "true" : text === "no" ? "false" : text;
    }
    default: return value === FILL_NEEDED && type.kind !== "string" ? null : String(value);
  }
}

// The `Command String` of the Python GDS page (command-string.js): the command of the form as one line,
// `NAME, ARG1, ARG2`, a string in double quotes, an array in brackets, a struct in braces, and
// `<FILL-VALUE>` where nothing is typed yet. A line typed there fills the form; it does not send.
const FILL_NEEDED = "<FILL-VALUE>";
const COMMAND_FORMAT_SPEC = "FULL_COMMAND_NAME[[[, ARG1], ARG2], ...] where ARGN is a decimal number, quoted string, or an enumerated constant";
// The expression of the Python page, as its template string leaves it: names get quotes, then the line is JSON.
const STRING_PREPROCESSOR = /(?:"((?:[^"]|")*)")|([a-zA-Z_][a-zA-Z_0-9.]*)|(<FILL-VALUE>)/g;

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
    if (type.kind !== "integer") return String(state);
    if (type.bits === 64) return String(BigInt(state));
    if (type.signed) return String(parseInt(state, 10));
    const radix = { "0x": 16, "0b": 2, "0o": 8 }[state.slice(0, 2)];
    return String(radix === 16 ? parseInt(state, 16) : radix ? parseInt(state.slice(2), radix) : parseInt(state, 10));
  } catch (e) { return FILL_NEEDED; }
}

// The tokens of a line: its name and its arguments, a number as the text the Python page gives its input
// (an integer of any size in full, any other number as JavaScript writes it).
function lineTokens(text) {
  const tokens = JSON.parse(`[${text.replace(STRING_PREPROCESSOR, '"$1$2$3"')}]`, (key, value, context) => {
    if (typeof value !== "number") return value;
    const source = context && context.source;
    return source && /^-?\d+$/.test(source) && !Number.isSafeInteger(value) ? source : String(value);
  });
  if (typeof tokens[0] !== "string") throw new SyntaxError("no command name");
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

// The mistake in a scalar input, or null: the message under the input ("" when the Python GDS page refuses
// without one), and whether the array or struct that holds the input names it in its own line.
function mistake(type, text) {
  const said = (message) => ({ text: message, counted: true });
  switch (type.kind) {
    // Integers and floats: the rules and the messages of the Python GDS page (validate.js).
    case "integer": {
      const pattern = type.signed ? /^(-?[1-9]\d*|0)$/
        : /^(0[bB][01]+|0[oO][0-7]+|0[xX][0-9a-fA-F]+|[1-9]\d*|0)$/;
      const min = type.signed ? -(1n << BigInt(type.bits - 1)) : 0n;
      const max = type.signed ? (1n << BigInt(type.bits - 1)) - 1n : (1n << BigInt(type.bits)) - 1n;
      const kind = type.signed ? "signed decimal integer" : "binary, octal, decimal, or hexadecimal unsigned integer";
      // validate.js reads the text with BigInt (which takes " 4 ", "007", "0x10" and ""): a message when
      // nothing was typed, when that fails or when the value is out of range. The pattern and `required` of
      // the input then refuse the rest without a message.
      let value = null;
      try { value = text === null ? null : BigInt(text); } catch (e) { /* not an integer */ }
      if (value === null || value < min || value > max) return said(`Supply ${kind}  between ${min} and ${max}`);
      return pattern.test(text) ? null : { text: "", counted: false };
    }
    case "float": return Number.isNaN(parseFloat(text)) ? said("Supply floating point number") : null;
    // validate.js has no message for a bool that is not chosen.
    case "bool": return text === "true" || text === "false" ? null : said("");
    case "string": {
      const bytes = new TextEncoder().encode(text).length;
      // The Python GDS page refuses an empty text and one longer than the type holds.
      const fits = bytes > 0 && (type.max === undefined || bytes <= type.max);
      return fits ? null : said(`Supply general text of less than ${type.max} characters`);
    }
    case "enum": return type.constants.some((c) => c.name === text) ? null : said(`Supply one of: ${type.constants.map((c) => c.name).join(" ")}`);
    default: return said(`type ${type.name} is not supported`);
  }
}

// The JSON value the API takes for an input state; mistakes are collected by path.
function convert(type, state, path, mistakes) {
  switch (type.kind) {
    case "array": return state.map((item, i) => convert(type.element, item, `${path}[${i}]`, mistakes));
    case "struct":
      return Object.fromEntries(type.members.map((m) => [
        m.name,
        m.count === null ? convert(m.type, state[m.name], `${path}.${m.name}`, mistakes)
          : state[m.name].map((item, i) => convert(m.type, item, `${path}.${m.name}[${i}]`, mistakes)),
      ]));
    default: {
      // A line typed as `Command String` can give an array or a struct where one value is taken.
      if (state !== null && typeof state === "object") state = String(state);
      const problem = mistake(type, state);
      if (problem !== null) mistakes.set(path, problem);
      if (type.kind === "bool") return state === "true";
      // Numbers go as typed: the GDS reads the text by the rule of the Python GDS.
      return state;
    }
  }
}

class CommandsTab extends LitElement {
  static properties = {
    filter: { state: true }, selected: { state: true }, values: { state: true }, mistakes: { state: true },
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
    this.mistakes = new Map();
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
    if (first) this.choose(first.name);
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

  choose(name, args) {
    const command = this.commands.find((c) => c.name === name);
    if (!command) return;
    this.selected = command;
    this.unknown = "";
    this.values = Object.fromEntries(command.params.map((p) => [
      p.name, args && p.name in args ? fromRecord(p.type, args[p.name]) : initial(p.type),
    ]));
    this.mistakes = new Map();
    this.message = null;
    this.cleared = true;
  }

  // A line typed as `Command String`: the form is filled and its fields are checked, as the Python page.
  fromLine(text, input) {
    this.lineError = "";
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
    // Fewer arguments than the command takes: the Python page stops while it fills the form. The command
    // shown keeps the arguments given when it is this one, another command stays as it is; nothing is checked.
    const missing = tokens.length - 1 < command.params.length;
    if (missing && this.selected !== command) return;
    this.updateComplete.then(() => { input.value = this.line; });
    // An argument the line does not give keeps what the form holds for this command.
    const held = this.selected === command ? this.values : {};
    this.choose(command.name, Object.fromEntries(command.params.map((p, i) => [p.name, tokens[i + 1]])
      .filter(([, value]) => value !== undefined)));
    this.values = { ...this.values, ...Object.fromEntries(command.params.slice(tokens.length - 1)
      .filter((p) => p.name in held).map((p) => [p.name, held[p.name]])) };
    if (!missing) this.request();
  }

  // Replace the value at `path` (argument name, then member names and indexes).
  setAt(path, value) {
    const next = structuredClone(this.values);
    let at = next;
    for (const key of path.slice(0, -1)) at = at[key];
    at[path[path.length - 1]] = value;
    this.values = next;
  }

  request() {
    const mistakes = new Map();
    const args = Object.fromEntries(this.selected.params.map((p) => [
      p.name, convert(p.type, this.values[p.name], p.name, mistakes),
    ]));
    this.mistakes = mistakes;
    return mistakes.size ? null : { name: this.selected.name, args };
  }

  async send(request = this.selected ? this.request() : null) {
    if (!this.selected && !request) return;
    if (!request) {
      // As the Python GDS page: the fields are marked, with their messages. Where it has no message for a
      // marked field (and shows the one of the browser), a line under the form says why nothing is sent.
      const silent = [...this.mistakes.values()].every((m) => m.text === "");
      this.message = silent
        ? { error: true, text: `${this.selected.name} was not sent: the marked fields are empty or not valid` } : null;
      return;
    }
    this.sending = true;
    try {
      const sent = await this.gds.api.post("/commands", request);
      this.message = { error: false, text: `${request.name} sent (record ${sent.seq})` };
    } catch (error) {
      this.message = { error: true, text: `${request.name} was not sent: ${error.message}` };
    } finally {
      this.sending = false;
    }
  }

  // The line the Python GDS page writes under an array or struct: one sentence per member in error.
  groupNote(key, children) {
    const keys = [...this.mistakes].filter(([, m]) => m.counted).map(([k]) => k);
    const bad = children.filter(([, child]) => keys.some((k) => k === child || k.startsWith(child + ".") || k.startsWith(child + "[")));
    const text = bad.map(([token]) => `Error in field/index: ${token}.`).join(" ");
    return bad.length ? html`<div class="error-text" data-error-for=${key}>${text}</div>` : nothing;
  }

  input(type, state, path, label) {
    const key = label;
    const problem = this.mistakes.has(key);
    const text = problem ? this.mistakes.get(key).text : "";
    const set = (e) => this.setAt(path, e.target.value);
    const note = text ? html`<span class="error-text" data-error-for=${key}>${text}</span>` : nothing;
    switch (type.kind) {
      case "enum":
        return html`<select data-arg=${key} class=${problem ? "invalid" : ""} @change=${set}>
          ${type.constants.map((c) => html`<option value=${c.name} ?selected=${c.name === state}>${c.name}</option>`)}
        </select>${note}`;
      case "bool":
        return html`<select data-arg=${key} class=${problem ? "invalid" : ""} @change=${set}>
          <option value="" ?selected=${!state}></option>
          <option value="true" ?selected=${String(state).toLowerCase() === "true"}>true</option>
          <option value="false" ?selected=${String(state).toLowerCase() === "false"}>false</option>
        </select>${note}`;
      case "array":
        return html`<div class="arg-group" data-arg-group=${key}>
          ${state.map((item, i) => html`<div class="arg-row"><span class="arg-name mono">[${i}]</span>
            ${this.input(type.element, item, [...path, i], `${key}[${i}]`)}</div>`)}
          ${this.groupNote(key, state.map((_, i) => [i, `${key}[${i}]`]))}
        </div>`;
      case "struct":
        return html`<div class="arg-group" data-arg-group=${key}>
          ${type.members.map((m) => html`<div class="arg-row"><span class="arg-name mono">${m.name}</span>
            ${m.count === null ? this.input(m.type, state[m.name], [...path, m.name], `${key}.${m.name}`)
              : html`<div class="arg-group">${state[m.name].map((item, i) => html`<div class="arg-row">
                  <span class="arg-name mono">[${i}]</span>
                  ${this.input(m.type, item, [...path, m.name, i], `${key}.${m.name}[${i}]`)}</div>`)}
                  ${this.groupNote(`${key}.${m.name}`, state[m.name].map((_, i) => [i, `${key}.${m.name}[${i}]`]))}</div>`}
          </div>`)}
          ${this.groupNote(key, type.members.map((m) => [m.name, `${key}.${m.name}`]))}
        </div>`;
      default:
        if (type.kind === "float") {
          // A number input, as in the Python GDS page. While the text is not yet a number ("1e", "-") the
          // input reports an empty value: `updated` then leaves the field alone, so the typing goes on.
          return html`<input type="number" step="any" data-arg=${key} class=${problem ? "invalid" : ""}
            .wanted=${state ?? ""} placeholder=${type.name} @input=${set}>${note}`;
        }
        return html`<input type="text" data-arg=${key}
          class=${problem ? "invalid" : ""} .value=${state ?? ""} placeholder=${type.name} @input=${set}>${note}`;
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
      && (!needle || c.name.toLowerCase().includes(needle) || format.hexId(c.opcode).includes(needle)));
    const line = command ? [command.name, ...command.params.map((p) => lineText(p.type, this.values[p.name]))].join(", ") : this.unknown;
    this.line = line;
    const wanted = this.historyFilter.toLowerCase();
    const history = this.history
      .filter((r) => !wanted || `${r.body.name} ${format.value(r.body.args)} ${r.body.outcome}`.toLowerCase().includes(wanted))
      .slice().reverse();
    return html`
      <div class="commanding">
        <div class="command-list">
          <input type="search" id="command-filter" placeholder="Filter commands" .value=${this.filter}
            @input=${(e) => { this.filter = e.target.value; }}>
          <label><input type="checkbox" id="show-parameter-commands" .checked=${this.parameters}
            @change=${(e) => { this.parameters = e.target.checked; }}>Show Parameter Commands</label>
          <select id="command-select" size="14" @change=${(e) => this.choose(e.target.value)}>
            ${listed.map((c) => html`<option value=${c.name} ?selected=${command && c.name === command.name}
              title=${c.annotation}>${c.name}</option>`)}
          </select>
        </div>
        <form class="command-form" id="command-form" novalidate @submit=${(e) => { e.preventDefault(); this.send(); }}>
          ${command ? html`
            <h3 id="command-name">${command.name} <span class="muted mono">${format.hexId(command.opcode)}</span></h3>
            <div class="muted">${command.annotation}</div>
            ${command.params.length ? nothing : html`<div class="muted">No arguments.</div>`}
            ${command.params.map((p) => html`<div class="arg-row top" data-param=${p.name}>
              <span class="arg-name" title=${p.annotation}>${p.name}<br><span class="muted mono">${p.type.name}</span></span>
              ${this.input(p.type, this.values[p.name], [p.name], p.name)}
            </div>`)}
            <div class="toolbar">
              <button class="action" id="command-send" type="submit" ?disabled=${this.sending}>Send command</button>
              <button class="action secondary" type="button" id="command-clear" @click=${() => this.choose(command.name)}>Clear</button>
            </div>` : this.unknown ? html`<h3 id="command-name">${this.unknown}</h3>
              <div class="error-text" id="command-unknown">${this.unknown} is not a command.</div>
              <div class="toolbar"><button class="action" id="command-send" type="submit">Send command</button></div>`
              : html`<div class="muted">Choose a command.</div>`}
          ${this.message ? html`<div id="command-message" class=${this.message.error ? "error-text" : ""}>${this.message.text}</div>` : nothing}
        </form>
      </div>
      <h3>Command String</h3>
      <input type="text" id="command-string" class=${this.lineError ? "invalid" : ""} .value=${line}
        placeholder="Command string of the form: ${COMMAND_FORMAT_SPEC}"
        @focus=${() => { this.lineError = ""; }} @change=${(e) => this.fromLine(e.target.value, e.target)}>
      ${this.lineError ? html`<div class="error-text" id="command-string-error">${this.lineError}</div>` : nothing}
      <h3>Command history</h3>
      <div class="toolbar">
        <label>Filter <input type="search" id="history-filter" .value=${this.historyFilter}
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
                @click=${() => this.choose(r.body.name, r.body.args)}>Load</button>
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
