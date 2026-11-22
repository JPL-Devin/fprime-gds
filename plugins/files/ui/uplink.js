// The Uplink tab: stage files, choose a destination, begin the uplink and control the queue (FIL-1, FIL-4).
import { FilesTab, html, styles, when, progress } from "./common.js";

class FgdsUplink extends FilesTab {
  static styles = styles;
  kind = "fprime.file.uplink";
  path = "/files/uplink";

  static properties = { ...FilesTab.properties, staged: { state: true } };

  constructor() {
    super();
    this.staged = []; // files chosen and not yet sent to the GDS: `NOT STARTED`, as in the Python GDS page
  }

  // As the Python page: the row keeps the Destination Folder text of the moment the file was added.
  stage(input) {
    const destination = this.renderRoot.querySelector("#directory").value.trim();
    this.staged = [...this.staged, ...Array.from(input.files, (file) => ({ file, destination }))];
    input.value = "";
  }

  // "Begin Uplink": every staged file goes to the queue of the GDS, which sends at once.
  async upload() {
    const directory = this.renderRoot.querySelector("#directory").value.trim();
    while (this.staged.length) {
      const file = this.staged[0].file;
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
        ${data.paused || !data.files.length
          ? html`<button id="unpause" ?disabled=${data.link !== "connected"} @click=${post("/resume")}>Unpause</button>`
          : html`<button id="pause" @click=${post("/pause")}>Pause</button>`}
        <button id="cancel-packet" @click=${post("/cancel-packet")}>Send Cancel Packet</button>
      </div>
      <div class="controls">
        <span>Queue: <b>${data.paused ? "paused" : "running"}</b>, link: ${data.link}</span>
        <span class="error">${this.error}</span>
      </div>
      <table>
        <tr><th>Start Time</th><th>End Time</th><th>Source</th><th>Destination</th><th>State</th><th>Control</th><th>Progress</th></tr>
        ${this.staged.map(({ file, destination }, n) => html`<tr class="staged">
          <td></td><td></td><td>${file.name}</td><td>${destination}</td><td>NOT STARTED</td>
          <td><button @click=${() => { this.staged = this.staged.filter((_, i) => i !== n); }}>Remove</button></td>
          <td>0%</td>
        </tr>`)}
        ${data.files.slice().reverse().map((t) => html`<tr>
          <td>${when(t.started)}</td><td>${when(t.ended)}</td><td>${t.source}</td><td>${t.destination}</td>
          <td class=${t.state}>${t.state}${t.reason ? ": " + t.reason : ""}</td>
          <td>${active(t)
            ? html`<button @click=${post("/" + encodeURIComponent(t.id) + "/cancel")}>
                ${t.state === "queued" ? "Remove" : "Cancel"}</button>`
            : ""}</td>
          <td>${progress(t)}</td>
        </tr>`)}
      </table>`;
  }
}

customElements.define("fgds-uplink", FgdsUplink);

export default { id: "uplink", title: "Uplink", element: "fgds-uplink" };
