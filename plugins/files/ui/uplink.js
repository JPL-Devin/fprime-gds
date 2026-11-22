// The Uplink tab: stage files, choose a destination, begin the uplink and control the queue (FIL-1, FIL-4).
import { FilesTab, html, styles, row } from "./common.js";

class FgdsUplink extends FilesTab {
  static styles = styles;
  kind = "fprime.file.uplink";
  path = "/files/uplink";

  static properties = { ...FilesTab.properties, staged: { state: true } };

  constructor() {
    super();
    this.staged = []; // files chosen and not yet sent to the GDS: `NOT STARTED`, as in the Python GDS page
  }

  stage(input) {
    this.staged = [...this.staged, ...input.files];
    input.value = "";
  }

  // "Begin Uplink": every staged file goes to the queue of the GDS, which sends at once.
  async upload() {
    const directory = this.renderRoot.querySelector("#directory").value;
    while (this.staged.length) {
      const file = this.staged[0];
      const query = new URLSearchParams({ name: file.name });
      // The destination is the name joined to the directory as typed (`uplink_destination`).
      query.set("directory", directory);
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
        ${data.paused
          ? html`<button id="unpause" ?disabled=${data.link !== "connected"} @click=${post("/resume")}>Unpause</button>`
          : html`<button id="pause" @click=${post("/pause")}>Pause</button>`}
        <button id="cancel-packet" @click=${post("/cancel-packet")}>Send Cancel Packet</button>
      </div>
      <div class="controls">
        <span>Queue: <b>${data.paused ? "paused" : "running"}</b>, link: ${data.link}</span>
        <span class="error">${this.error}</span>
      </div>
      <table>
        <tr><th>Start time</th><th>End time</th><th>File</th><th>Source</th><th>Destination</th><th>State</th><th>Progress</th><th></th></tr>
        ${data.files.map((t) => row(t, active(t)
          ? html`<button @click=${post("/" + encodeURIComponent(t.id) + "/cancel")}>
              ${t.state === "queued" ? "Remove" : "Cancel"}</button>`
          : ""))}
        ${this.staged.map((file, n) => html`<tr class="staged">
          <td></td><td></td><td>${file.name}</td><td>${file.name}</td><td></td><td>NOT STARTED</td>
          <td>0 / ${file.size} bytes (0%)</td>
          <td><button @click=${() => { this.staged = this.staged.filter((_, i) => i !== n); }}>Remove</button></td>
        </tr>`)}
      </table>`;
  }
}

customElements.define("fgds-uplink", FgdsUplink);

export default { id: "uplink", title: "Uplink", element: "fgds-uplink" };
