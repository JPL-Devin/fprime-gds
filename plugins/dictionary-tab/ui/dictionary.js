// The Dictionaries tab: what the dictionary of the run defines, with a search.
import { LitElement, html } from "/ui/lit.js";

// A description on one line, as the Python GDS page shows it (HTML folds the line breaks of the dictionary).
const note = (text) => (text || "").replace(/\s+/g, " ").trim();

// The name of a type as the dictionary writes it.
const typeName = (type) => (type ? type.name || type.qualifiedName || type.kind : "");
const params = (list) => (list || []).map((p) => `${p.name}: ${typeName(p.type)}`).join(", ");

const SECTIONS = [
  {
    id: "commands", title: "Commands", source: (d) => d.commands,
    columns: ["Opcode", "Name", "Arguments", "Description"],
    row: (c, f) => [f.hexId(c.opcode), c.name, params(c.formalParams), note(c.annotation)],
  },
  {
    id: "events", title: "Events", source: (d) => d.events,
    columns: ["Id", "Name", "Severity", "Arguments", "Format", "Description"],
    row: (e, f) => [f.hexId(e.id), e.name, e.severity, params(e.formalParams), e.format || "", note(e.annotation)],
  },
  {
    id: "channels", title: "Channels", source: (d) => d.telemetryChannels,
    columns: ["Id", "Name", "Type", "Update", "Format", "Limits", "Description"],
    row: (c, f) => [f.hexId(c.id), c.name, typeName(c.type), c.telemetryUpdate || "", c.format || "",
      c.limits ? f.value(c.limits) : "", note(c.annotation)],
  },
  {
    id: "parameters", title: "Parameters", source: (d) => d.parameters,
    columns: ["Id", "Name", "Type", "Default", "Description"],
    row: (p, f) => [f.hexId(p.id), p.name, typeName(p.type), p.default === undefined ? "" : f.value(p.default),
      note(p.annotation)],
  },
  {
    id: "types", title: "Types", source: (d) => d.typeDefinitions,
    columns: ["Name", "Kind", "Definition", "Description"],
    row: (t, f) => [t.qualifiedName, t.kind, definition(t, f), note(t.annotation)],
  },
  {
    id: "packets", title: "Telemetry packets",
    source: (d) => (d.telemetryPacketSets || []).flatMap((set) => set.members.map((p) => ({ set: set.name, ...p }))),
    columns: ["Set", "Id", "Name", "Group", "Channels"],
    row: (p) => [p.set, String(p.id), p.name, String(p.group), p.members.join(", ")],
  },
];

function definition(type, f) {
  switch (type.kind) {
    case "enum":
      return `${typeName(type.representationType)}: ` + type.enumeratedConstants.map((c) => `${c.name} = ${c.value}`).join(", ");
    case "array": return `${typeName(type.elementType)}[${type.size}]`;
    case "struct":
      return Object.entries(type.members).sort((a, b) => a[1].index - b[1].index)
        .map(([name, m]) => `${name}: ${typeName(m.type)}${m.size ? `[${m.size}]` : ""}`).join(", ");
    case "alias": return `${typeName(type.type)} (${typeName(type.underlyingType)})`;
    default: return f.value(type);
  }
}

class DictionaryTab extends LitElement {
  static properties = { section: { state: true }, text: { state: true } };

  constructor() {
    super();
    this.section = "commands";
    this.text = "";
  }

  createRenderRoot() { return this; }

  render() {
    const dictionary = this.gds.dictionary;
    const meta = dictionary.metadata || {};
    const section = SECTIONS.find((s) => s.id === this.section);
    const all = (section.source(dictionary) || []).map((item) => section.row(item, this.gds.format));
    const rows = all.filter((row) => this.gds.format.matches(row, this.text));
    return html`
      <dl class="facts" id="dictionary-version">
        <dt>Deployment</dt><dd>${meta.deploymentName || ""}</dd>
        <dt>Project version</dt><dd data-field="project-version">${meta.projectVersion || ""}</dd>
        <dt>Framework version</dt><dd>${meta.frameworkVersion || ""}</dd>
        <dt>Library versions</dt><dd>${(meta.libraryVersions || []).join(", ") || "none"}</dd>
        <dt>Dictionary specification</dt><dd>${meta.dictionarySpecVersion || ""}</dd>
      </dl>
      <div class="subtabs" role="tablist">
        ${SECTIONS.map((s) => html`<button role="tab" data-section=${s.id} aria-selected=${String(s.id === this.section)}
          @click=${() => { this.section = s.id; }}>${s.title} (${(s.source(dictionary) || []).length})</button>`)}
      </div>
      <h2>${SECTIONS.find((s) => s.id === this.section).title} Dictionary</h2>
      <div class="toolbar">
        <label>Filters: <input type="search" id="dictionary-search" .value=${this.text}
          @input=${(e) => { this.text = e.target.value; }}></label>
        <span class="muted" id="dictionary-count">${rows.length} of ${all.length}</span>
      </div>
      <table class="gds" id="dictionary-table">
        <thead><tr>${section.columns.map((c) => html`<th>${c}</th>`)}</tr></thead>
        <tbody>${rows.map((row) => html`<tr>${row.map((cell, i) => html`<td class=${i < 2 ? "nowrap" : "text"}>${cell}</td>`)}</tr>`)}</tbody>
      </table>`;
  }
}
customElements.define("dictionary-tab", DictionaryTab);

export default { id: "dictionary", title: "Dictionaries", element: "dictionary-tab" };
