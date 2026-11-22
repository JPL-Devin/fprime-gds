// The Uplink tab: stage files, choose a destination, begin the uplink and control the queue (FIL-1, FIL-4).
import { FilesTab, html, styles, row } from "./common.js";

class FgdsUplink extends FilesTab {
  static styles = styles;
  kind = "fprime.file.uplink";
  path = "/files/uplink";

  static properties = { ...FilesTab.properties, staged: { state: true }, editing: { state: true }, packetError: { state: true } };

  constructor() {
    super();
    this.staged = []; // files chosen and not yet sent to the GDS: `NOT STARTED`, as in the Python GDS page
  }

  // As the Python page: the row keeps the Destination Folder text of the moment the file was added.
  stage(input) {
    const destination = this.renderRoot.querySelector("#directory").value.trim();
    this.staged = [...this.staged, ...Array.from(input.files, (file) => ({ file, destination, packets: [] }))];
    input.value = "";
  }

  // The packet numbers typed in "Uplink File Packet List", or null with the message of the Python GDS page.
  packetNumbers(text) {
    const tokens = text.split(",").map((token) => token.trim()).filter(Boolean);
    const valid = tokens.every((token) => /^[1-9]\d*$/.test(token));
    this.packetError = valid ? "" : "Specify packet numbers as comma-separated list";
    return valid ? tokens.map((token) => parseInt(token, 10)) : null;
  }

  savePackets() {
    const numbers = this.packetNumbers(this.renderRoot.querySelector("#packetList").value);
    if (numbers === null) return;
    this.editing.packets = numbers;
    this.editing = null;
  }

  // "Begin Uplink": every staged file goes to the queue of the GDS, which sends at once.
  async upload() {
    const directory = this.renderRoot.querySelector("#directory").value.trim();
    while (this.staged.length) {
      const file = this.staged[0].file;
      const query = new URLSearchParams({ name: file.name });
      // The destination is the name joined to the directory as typed (`uplink_destination`).
      query.set("directory", directory);
      if (this.staged[0].packets.length) query.set("packets", this.staged[0].packets.join(","));
      // The body is the file itself: the shared `api.post` sends JSON only.
      const response = await fetch("/api/v1/files/uplink?" + query, { method: "POST", body: file });
      if (!response.ok) {
        const body = await response.json().catch(() => ({}));
        throw new Error(file.name + ": " + (body.error ? body.error.message : response.statusText));
      }
      this.staged = this.staged.slice(1);
    }
  }

  render() {
    const data = this.data || { files: [], paused: false, link: "" };
    const post = (path) => () => this.act(() => this.gds.api.post("/files/uplink" + path, {}));
    const active = (t) => t.state === "queued" || t.state === "transferring";
    return html`
      <div class="controls">
        <label>Destination Folder <input id="directory" placeholder="on the spacecraft"></label>
        <label class="button">Add Files
          <input id="files" type="file" multiple hidden @change=${(e) => this.stage(e.target)}></label>
        <button id="begin" @click=${() => this.act(() => this.upload())}>Begin Uplink</button>
        ${data.paused || !data.files.length
          ? html`<button id="unpause" ?disabled=${data.link !== "connected"} @click=${post("/resume")}>Unpause</button>`
          : html`<button id="pause" @click=${post("/pause")}>Pause</button>`}
        <button id="cancel-packet" ?disabled=${data.files.some((t) => t.state === "transferring")} @click=${post("/cancel-packet")}>Send Cancel Packet</button>
      </div>
      <div class="controls">
        <span>Queue: <b>${data.paused ? "paused" : "running"}</b>, link: ${data.link}</span>
        <span class="error">${this.error}</span>
      </div>
      ${this.editing ? html`<div class="packets" id="packet-selection">
        <h3>Uplink File Packet List</h3>
        <h4>Selecting Packets for: ${this.editing.file.name}</h4>
        <label for="packetList">Enumerate specific file packets to uplink in a comma-separated list</label>
        <textarea id="packetList" placeholder="List specific packets to uplink: 1, 5, 9..." .value=${this.editing.packets.join(", ")}
          @input=${(e) => this.packetNumbers(e.target.value)}></textarea>
        <span class="error">${this.packetError}</span>
        <button id="packets-save" ?disabled=${this.packetError !== ""} @click=${() => this.savePackets()}>Save</button>
      </div>` : ""}
      <table>
        <tr><th>Start Time</th><th>End Time</th><th>Source</th><th>Destination</th><th>State</th><th>Control</th><th>Progress</th></tr>
        ${this.staged.map((item, n) => html`<tr class="staged">
          <td></td><td></td><td>${item.file.name}${item.packets.length ? ` [${item.packets.join(", ")}]` : ""}</td>
          <td>${item.destination}</td><td>NOT STARTED</td>
          <td><button @click=${() => { this.staged = this.staged.filter((_, i) => i !== n); }}>Remove</button>
            <button class="select-packets" @click=${() => { this.packetError = ""; this.editing = item; }}>Select Packets</button></td>
          <td>0%</td>
        </tr>`)}
        ${data.files.slice().reverse().map((t) => row(t, active(t)
          ? html`<button @click=${post("/" + encodeURIComponent(t.id) + "/cancel")}>
              ${t.state === "queued" ? "Remove" : "Cancel"}</button>`
          : ""))}
      </table>`;
  }
}

customElements.define("fgds-uplink", FgdsUplink);

export default { id: "uplink", title: "Uplink", element: "fgds-uplink" };
