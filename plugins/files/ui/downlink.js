// The Downlink tab: the transfers of this run and the files that can be downloaded (FIL-2).
import { FilesTab, html, styles, row, cells } from "./common.js";

class FgdsDownlink extends FilesTab {
  static styles = styles;
  kind = "fprime.file.downlink";
  path = "/files/downlink";

  render() {
    const data = this.data || { transfers: [], files: [] };
    const link = (name) => html`<a href=${"/api/v1/files/downlink/" + encodeURIComponent(name)} download=${name}>Download</a>`;
    return html`
      <span class="error">${this.error}</span>
      <h2>File Downlink</h2>
      <div class="controls">${this.filterField()}</div>
      <table id="transfers">
        ${this.head(["Start Time", "End Time", "Source", "Destination", "State", "Control", "Progress", "Problems"])}
        ${this.arrange(data.transfers, (t) => [...cells(t), t.problems || ""]).map((t) => row(t, t.state === "finished" ? link(t.name) : "", t.problems || ""))}
      </table>
      <h3>Files</h3>
      <table>
        <tr><th>File</th><th>Size</th><th></th></tr>
        ${data.files.map((f) => html`<tr><td>${f.name}</td><td>${f.size} bytes</td><td>${link(f.name)}</td></tr>`)}
      </table>`;
  }
}

customElements.define("fgds-downlink", FgdsDownlink);

export default { id: "downlink", title: "Downlink", element: "fgds-downlink" };
