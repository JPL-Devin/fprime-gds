// The Uplink tab: choose files, a destination, and control the queue (FIL-1, FIL-4).
import { FilesTab, html, styles, row } from "./common.js";

class FgdsUplink extends FilesTab {
  static styles = styles;
  kind = "fprime.file.uplink";
  path = "/files/uplink";

  async upload() {
    const input = this.renderRoot.querySelector("#files");
    const directory = this.renderRoot.querySelector("#directory").value;
    for (const file of input.files) {
      const query = new URLSearchParams({ name: file.name });
      if (directory) query.set("destination", directory.replace(/\/+$/, "") + "/" + file.name);
      // The body is the file itself: the shared `api.post` sends JSON only.
      const response = await fetch("/api/v1/files/uplink?" + query, { method: "POST", body: file });
      if (!response.ok) {
        const body = await response.json().catch(() => ({}));
        throw new Error(file.name + ": " + (body.error ? body.error.message : response.statusText));
      }
    }
    input.value = "";
  }

  render() {
    const data = this.data || { files: [], paused: false, link: "" };
    const post = (path) => () => this.act(() => this.gds.api.post("/files/uplink" + path, {}));
    const active = (t) => t.state === "queued" || t.state === "transferring";
    return html`
      <div class="controls">
        <input id="files" type="file" multiple>
        <label>Destination directory <input id="directory" placeholder="on the spacecraft"></label>
        <button @click=${() => this.act(() => this.upload())}>Add to queue</button>
      </div>
      <div class="controls">
        <span>Queue: <b>${data.paused ? "paused" : "running"}</b>, link: ${data.link}</span>
        <button ?disabled=${!data.paused || data.link !== "connected"} @click=${post("/resume")}>Resume</button>
        <button ?disabled=${data.paused} @click=${post("/pause")}>Pause</button>
        <span class="error">${this.error}</span>
      </div>
      <table>
        <tr><th>Start time</th><th>End time</th><th>File</th><th>Source</th><th>Destination</th><th>State</th><th>Progress</th><th></th></tr>
        ${data.files.map((t) => row(t, active(t)
          ? html`<button @click=${post("/" + encodeURIComponent(t.id) + "/cancel")}>
              ${t.state === "queued" ? "Remove" : "Cancel"}</button>`
          : ""))}
      </table>`;
  }
}

customElements.define("fgds-uplink", FgdsUplink);

export default { id: "uplink", title: "Uplink", element: "fgds-uplink" };
