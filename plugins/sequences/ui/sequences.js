// The Sequences tab: write a command sequence, build its binary with the compiler of
// `fgds seqgen`, and optionally send the binary through the uplink queue of the files plugin.
import { LitElement, html } from "/ui/lit.js";

const START = "; A command sequence: R<hh:mm:ss> or A<year-dayThh:mm:ss> MNEMONIC arguments\n";

class SequencesTab extends LitElement {
  static properties = { message: { state: true }, failed: { state: true }, busy: { state: true } };

  constructor() {
    super();
    this.message = "";
    this.failed = false;
    this.busy = false;
  }

  createRenderRoot() { return this; }

  field(id) { return this.querySelector("#" + id); }

  async build(uplink) {
    this.busy = true;
    this.failed = false;
    this.message = "";
    try {
      const name = this.field("sequence-name").value.trim() || "sequence.seq";
      const text = this.field("sequence-text").value;
      this.gds.settings.set("sequences.text", text);
      this.gds.settings.set("sequences.name", name);
      const built = await this.gds.api.post("/sequences/compile", { name, text });
      const bytes = Uint8Array.from(built.hex.match(/../g) || [], (pair) => parseInt(pair, 16));
      const binary = name.replace(/\.[^./]*$/, "") + ".bin";
      this.message = `${binary}: ${built.records} bytes of records, ${built.size} bytes, CRC ${built.crc}`;
      if (uplink) {
        const directory = this.field("sequence-directory").value.replace(/\/+$/, "");
        const query = new URLSearchParams({ name: binary });
        if (directory) query.set("destination", directory + "/" + binary);
        const response = await fetch("/api/v1/files/uplink?" + query, { method: "POST", body: bytes });
        if (!response.ok) {
          const body = await response.json().catch(() => ({}));
          throw new Error(body.error ? body.error.message : response.statusText);
        }
        this.message += "; added to the uplink queue";
      } else {
        const link = document.createElement("a");
        link.href = URL.createObjectURL(new Blob([bytes]));
        link.download = binary;
        link.click();
        URL.revokeObjectURL(link.href);
      }
    } catch (e) {
      this.failed = true;
      this.message = e.message;
    }
    this.busy = false;
  }

  // Set through the style object: the content security policy of the page allows no style attribute.
  firstUpdated() {
    Object.assign(this.field("sequence-text").style, { width: "100%", fontFamily: "monospace" });
  }

  render() {
    const settings = this.gds.settings;
    return html`
      <div class="toolbar">
        <label>Name <input id="sequence-name" .value=${settings.get("sequences.name", "sequence.seq")}></label>
        <label>Destination directory <input id="sequence-directory" placeholder="on the spacecraft" value="/seq"></label>
        <button id="sequence-build" ?disabled=${this.busy} @click=${() => this.build(false)}>Build and download</button>
        <button id="sequence-uplink" ?disabled=${this.busy} @click=${() => this.build(true)}>Build and uplink</button>
      </div>
      <textarea id="sequence-text" rows="24" spellcheck="false"
        .value=${settings.get("sequences.text", START)}></textarea>
      <pre id="sequence-result" class=${this.failed ? "error" : ""}>${this.message}</pre>`;
  }
}

customElements.define("fgds-sequences", SequencesTab);

export default { id: "sequences", title: "Sequences", element: "fgds-sequences" };
