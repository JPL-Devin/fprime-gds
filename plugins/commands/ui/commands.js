// The Commanding tab: a command chosen from the dictionary, one input per argument by
// its type (enum, array and struct included), send, and the history of commands sent.
import { LitElement, html, nothing } from "/ui/lit.js";

const KIND = "fprime.command";

// The value an input of `type` starts with. Scalars start empty, so nothing is sent by accident.
function initial(type) {
  switch (type.kind) {
    case "enum": return type.constants.some((c) => c.name === type.default) ? type.default : type.constants[0].name;
    case "bool": return "";
    case "array": return Array.from({ length: type.size }, () => initial(type.element));
    case "struct":
      return Object.fromEntries(type.members.map((m) => [
        m.name, m.count === null ? initial(m.type) : Array.from({ length: m.count }, () => initial(m.type)),
      ]));
    default: return "";
  }
}

// The input state for a value taken from a command record (resend).
function fromRecord(type, value) {
  if (value === null || value === undefined) return initial(type);
  switch (type.kind) {
    case "array": return Array.from({ length: type.size }, (_, i) => fromRecord(type.element, (value || [])[i]));
    case "struct":
      return Object.fromEntries(type.members.map((m) => {
        const v = value[m.name];
        return [m.name, m.count === null ? fromRecord(m.type, v)
          : Array.from({ length: m.count }, (_, i) => fromRecord(m.type, (v || [])[i]))];
      }));
    default: return String(value);
  }
}

// The mistake in a scalar input, or null.
function mistake(type, text) {
  switch (type.kind) {
    case "integer": {
      if (!/^\s*(-?\d+|0[xX][0-9a-fA-F]+)\s*$/.test(text)) return `not an integer (${type.name})`;
      const v = BigInt(text.trim());
      const min = type.signed ? -(1n << BigInt(type.bits - 1)) : 0n;
      const max = type.signed ? (1n << BigInt(type.bits - 1)) - 1n : (1n << BigInt(type.bits)) - 1n;
      return v < min || v > max ? `out of range for ${type.name} (${min} to ${max})` : null;
    }
    case "float":
      if (["NaN", "Infinity", "-Infinity"].includes(text.trim())) return null;
      return text.trim() === "" || Number.isNaN(Number(text)) ? `not a number (${type.name})` : null;
    case "bool": return text === "true" || text === "false" ? null : "choose true or false";
    case "string": {
      const bytes = new TextEncoder().encode(text).length;
      return type.max !== undefined && bytes > type.max ? `${bytes} bytes, at most ${type.max}` : null;
    }
    case "enum": return type.constants.some((c) => c.name === text) ? null : `not a constant of ${type.name}`;
    default: return `type ${type.name} is not supported`;
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
      const problem = mistake(type, state);
      if (problem) mistakes.set(path, problem);
      if (type.kind === "bool") return state === "true";
      if (type.kind === "float") return ["NaN", "Infinity", "-Infinity"].includes(state.trim()) ? state.trim() : Number(state);
      // Integers go as text: a 64-bit value does not fit a JSON number in the browser.
      return type.kind === "integer" ? state.trim() : state;
    }
  }
}

class CommandsTab extends LitElement {
  static properties = {
    filter: { state: true }, selected: { state: true }, values: { state: true }, mistakes: { state: true },
    message: { state: true }, sending: { state: true }, version: { state: true }, historyFilter: { state: true },
  };

  constructor() {
    super();
    this.filter = "";
    this.historyFilter = "";
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
    this.commands = (this.gds.dictionary.commands || [])
      .map((c) => ({
        name: c.name, opcode: c.opcode, annotation: c.annotation || "",
        params: c.formalParams.map((p) => ({ name: p.name, annotation: p.annotation || "", type: types.resolve(p.type) })),
      }))
      .sort((a, b) => a.name.localeCompare(b.name));
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

  choose(name, args) {
    const command = this.commands.find((c) => c.name === name);
    if (!command) return;
    this.selected = command;
    this.values = Object.fromEntries(command.params.map((p) => [
      p.name, args && p.name in args ? fromRecord(p.type, args[p.name]) : initial(p.type),
    ]));
    this.mistakes = new Map();
    this.message = null;
  }

  // Replace the value at `path` (argument name, then member names and indexes).
  update(path, value) {
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

  async send(request = this.request()) {
    if (!request) {
      this.message = { error: true, text: "The arguments are not valid; nothing was sent." };
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

  input(type, state, path, label) {
    const key = label;
    const problem = this.mistakes.get(key);
    const set = (e) => this.update(path, e.target.value);
    const note = problem ? html`<span class="error-text" data-error-for=${key}>${problem}</span>` : nothing;
    switch (type.kind) {
      case "enum":
        return html`<select data-arg=${key} class=${problem ? "invalid" : ""} @change=${set}>
          ${type.constants.map((c) => html`<option value=${c.name} ?selected=${c.name === state}>${c.name}</option>`)}
        </select>${note}`;
      case "bool":
        return html`<select data-arg=${key} class=${problem ? "invalid" : ""} @change=${set}>
          <option value="" ?selected=${state === ""}></option>
          <option value="true" ?selected=${String(state).toLowerCase() === "true"}>true</option>
          <option value="false" ?selected=${String(state).toLowerCase() === "false"}>false</option>
        </select>${note}`;
      case "array":
        return html`<div class="arg-group" data-arg-group=${key}>
          ${state.map((item, i) => html`<div class="arg-row"><span class="arg-name mono">[${i}]</span>
            ${this.input(type.element, item, [...path, i], `${key}[${i}]`)}</div>`)}
        </div>`;
      case "struct":
        return html`<div class="arg-group" data-arg-group=${key}>
          ${type.members.map((m) => html`<div class="arg-row"><span class="arg-name mono">${m.name}</span>
            ${m.count === null ? this.input(m.type, state[m.name], [...path, m.name], `${key}.${m.name}`)
              : html`<div class="arg-group">${state[m.name].map((item, i) => html`<div class="arg-row">
                  <span class="arg-name mono">[${i}]</span>
                  ${this.input(m.type, item, [...path, m.name, i], `${key}.${m.name}[${i}]`)}</div>`)}</div>`}
          </div>`)}
        </div>`;
      default:
        return html`<input type="text" data-arg=${key} class=${problem ? "invalid" : ""} .value=${state}
          placeholder=${type.name} @input=${set}>${note}`;
    }
  }

  render() {
    const format = this.gds.format;
    const needle = this.filter.toLowerCase();
    const listed = this.commands.filter((c) => !needle || c.name.toLowerCase().includes(needle)
      || format.hexId(c.opcode).includes(needle));
    const command = this.selected;
    const wanted = this.historyFilter.toLowerCase();
    const history = this.history
      .filter((r) => !wanted || `${r.body.name} ${format.value(r.body.args)} ${r.body.outcome}`.toLowerCase().includes(wanted))
      .slice().reverse();
    return html`
      <div class="commanding">
        <div class="command-list">
          <input type="search" id="command-filter" placeholder="Filter commands" .value=${this.filter}
            @input=${(e) => { this.filter = e.target.value; }}>
          <select id="command-select" size="14" @change=${(e) => this.choose(e.target.value)}>
            ${listed.map((c) => html`<option value=${c.name} ?selected=${command && c.name === command.name}
              title=${c.annotation}>${c.name}</option>`)}
          </select>
        </div>
        <form class="command-form" id="command-form" @submit=${(e) => { e.preventDefault(); if (command) this.send(); }}>
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
            </div>` : html`<div class="muted">Choose a command.</div>`}
          ${this.message ? html`<div id="command-message" class=${this.message.error ? "error-text" : ""}>${this.message.text}</div>` : nothing}
        </form>
      </div>
      <h3>Command history</h3>
      <div class="toolbar">
        <label>Filter <input type="search" id="history-filter" .value=${this.historyFilter}
          @input=${(e) => { this.historyFilter = e.target.value; }}></label>
        <span class="muted">${history.length} of ${this.history.length} commands; a row can be loaded into the form or sent again.</span>
      </div>
      <table class="gds" id="command-history">
        <thead><tr><th>Ground time</th><th>Opcode</th><th>Command</th><th>Arguments</th><th>Outcome</th><th></th></tr></thead>
        <tbody>
          ${history.map((r) => html`<tr class="outcome-${r.body.outcome}" data-seq=${r.seq}>
            <td class="nowrap mono" data-col="ground">${format.groundTime(r.ground_time)}</td>
            <td class="nowrap mono" data-col="opcode">${format.hexId(r.id)}</td>
            <td class="nowrap" data-col="name">${r.body.name}</td>
            <td class="text" data-col="args">${format.value(Object.values(r.body.args || {}))}</td>
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
