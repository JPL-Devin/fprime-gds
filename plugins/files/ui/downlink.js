// The Downlink tab: the transfers of this run and the files that can be downloaded (FIL-2).
import { FilesTab, html, styles, row } from "./common.js";

class FgdsDownlink extends FilesTab {
  static styles = styles;
  kind = "fprime.file.downlink";
  path = "/files/downlink";

  render() {
    const data = this.data || { transfers: [], files: [] };
    const link = (name) => html`<a href=${"/api/v1/files/downlink/" + encodeURIComponent(name)} download=${name}>Download</a>`;
    return html`
      <span class="error">${this.error}</span>
      <h3>Transfers</h3>
      <table>
        <tr><th>Start time</th><th>End time</th><th>File</th><th>Source</th><th>Destination</th><th>State</th><th>Progress</th><th>Problems</th></tr>
        ${data.transfers.map((t) => row(t, t.problems || ""))}
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
