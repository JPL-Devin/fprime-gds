// The Uplink tab: stage files, choose a destination, begin the uplink and control the queue (FIL-1, FIL-4).
import { FilesTab, html, styles, row, cells } from "./common.js";

class FgdsUplink extends FilesTab {
  static styles = styles;
  kind = "fprime.file.uplink";
  path = "/files/uplink";

  static properties = { ...FilesTab.properties, staged: { state: true }, refused: { state: true }, notices: { state: true }, editing: { state: true }, packetError: { state: true } };

  constructor() {
    super();
    this.refused = []; // files the GDS refused at Begin Uplink, with the reason
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
    this.notices = [];
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
        const reason = body.error ? body.error.message : response.statusText;
        // A file the GDS refuses for what it is (its name, its destination, its size, its packets) leaves
        // the staged rows and is named with the reason; the files after it are still sent. Anything else
        // (no GDS, a queue that takes no file now) stops here with the file still staged.
        if (response.status !== 422 && response.status !== 413) throw new Error(file.name + ": " + reason);
        this.refused = [...this.refused, { name: file.name, reason }];
        this.staged = this.staged.slice(1);
        continue;
      }
      const notice = (await response.json().catch(() => ({}))).notice;
      if (notice) this.notices = [...(this.notices || []), notice];
      this.staged = this.staged.slice(1);
    }
  }

  render() {
    const data = this.data || { files: [], paused: false, link: "" };
    const post = (path) => () => this.act(() => this.gds.api.post("/files/uplink" + path, {}));
    const active = (t) => t.state === "queued" || t.state === "transferring";
    return html`
      <h2>File Uplink</h2>
      <div class="controls">
        <label>Destination Folder <input id="directory" title="Destination folder path on the spacecraft where files will be uploaded" placeholder="/path/to/destination/folder"></label>
        <label class="button" title="Select files to upload">Add Files
          <input id="files" type="file" multiple hidden @change=${(e) => this.stage(e.target)}></label>
        <button id="begin" title="Begin uplinking all uploaded files" @click=${() => this.act(() => this.upload())}>Begin Uplink</button>
        ${data.paused || !data.files.length
          ? html`<button id="unpause" title="Resume file uplink" ?disabled=${data.link !== "connected"} @click=${post("/resume")}>Unpause</button>`
          : html`<button id="pause" title="Pause file uplink" @click=${post("/pause")}>Pause</button>`}
        <button id="cancel-packet" title="Send a cancel packet to FSW" ?disabled=${data.files.some((t) => t.state === "transferring")} @click=${post("/cancel-packet")}>Send Cancel Packet</button>
      </div>
      <div class="controls">
        <span>Queue: <b>${data.paused ? "paused" : "running"}</b>, link: ${data.link}</span>
        <span class="error">${this.error}</span>
      </div>
      ${(this.notices || []).map((notice) => html`<p class="note uplink-notice">${notice}</p>`)}
      ${this.refused.map((item, n) => html`<p class="error uplink-refused">REFUSED, not uplinked: ${item.name}: ${item.reason}
        <button title="Remove this line" @click=${() => { this.refused = this.refused.filter((_, i) => i !== n); }}>Dismiss</button></p>`)}
      ${this.editing ? html`<div class="packets" id="packet-selection">
        <h3>Uplink File Packet List</h3>
        <h4>Selecting Packets for: ${this.editing.file.name}</h4>
        <label for="packetList">Enumerate specific file packets to uplink in a comma-separated list</label>
        <textarea id="packetList" placeholder="List specific packets to uplink: 1, 5, 9..." .value=${this.editing.packets.join(", ")}
          @input=${(e) => this.packetNumbers(e.target.value)}></textarea>
        <span class="error">${this.packetError}</span>
        <button id="packets-save" title="Save" ?disabled=${this.packetError !== ""} @click=${() => this.savePackets()}>Save</button>
      </div>` : ""}
      <div class="controls">${this.filterField()}</div>
      <table id="transfers">
        ${this.head(["Start Time", "End Time", "Source", "Destination", "State", "Control", "Progress"])}
        ${this.arrange([
          ...this.staged.map((item, n) => ({ item, n })),
          ...data.files.slice().reverse().map((t) => ({ t })),
        ], (entry) => (entry.t ? cells(entry.t) : ["", "", entry.item.file.name, entry.item.destination, "NOT STARTED", "", "0%"]))
          .map(({ item, n, t }) => (item ? html`<tr class="staged">
          <td></td><td></td><td>${item.file.name}${item.packets.length ? ` [${item.packets.join(", ")}]` : ""}</td>
          <td>${item.destination}</td><td>NOT STARTED</td>
          <td><button @click=${() => { this.staged = this.staged.filter((_, i) => i !== n); }}>Remove</button>
            <button class="select-packets" @click=${() => { this.packetError = ""; this.editing = item; }}>Select Packets</button></td>
          <td>0%</td>
        </tr>` : row(t, active(t)
          ? html`<button @click=${post("/" + encodeURIComponent(t.id) + "/cancel")}>
              ${t.state === "queued" ? "Remove" : "Cancel"}</button>`
          : "")))}
      </table>`;
  }
}

customElements.define("fgds-uplink", FgdsUplink);

export default { id: "uplink", title: "Uplink", element: "fgds-uplink" };
