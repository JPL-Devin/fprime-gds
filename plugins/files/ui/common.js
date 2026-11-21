// What the Uplink and Downlink tabs share. Every value is rendered by Lit as text (UI-4).
import { LitElement, html, css } from "./lit-core.min.js";

export { LitElement, html, css };

export const styles = css`
  :host { display: block; padding: 0.5rem; }
  table { border-collapse: collapse; width: 100%; }
  th, td { text-align: left; padding: 0.2rem 0.6rem; border-bottom: 1px solid #ccc; }
  .failed, .error { color: #b00020; }
  .controls { display: flex; gap: 0.5rem; align-items: center; margin-bottom: 0.5rem; flex-wrap: wrap; }
  progress { width: 8rem; }
`;

// A tab that keeps `this.data` equal to one GET, refreshed when a record of `kind` arrives.
export class FilesTab extends LitElement {
  static properties = { gds: {}, data: { state: true }, error: { state: true } };

  connectedCallback() {
    super.connectedCallback();
    this.refresh();
    // Records tell when something changed; the route gives the whole state.
    this.unsubscribe = this.gds.records.subscribe({ kinds: [this.kind] }, () => this.refresh());
    this.timer = setInterval(() => this.refresh(), 2000);
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    if (this.unsubscribe) this.unsubscribe();
    clearInterval(this.timer);
  }

  async refresh() {
    try {
      this.data = await this.gds.api.get(this.path);
    } catch (e) {
      this.error = e.message;
    }
  }

  async act(action) {
    this.error = "";
    try {
      await action();
    } catch (e) {
      this.error = e.message;
    }
    this.refresh();
  }
}

export function row(t, extra) {
  return html`<tr>
    <td>${t.name}</td>
    <td>${t.source}</td>
    <td>${t.destination}</td>
    <td class=${t.state}>${t.state}${t.reason ? ": " + t.reason : ""}</td>
    <td><progress max=${t.size || 1} value=${t.size ? t.done : (t.state === "finished" ? 1 : 0)}></progress>
      ${t.done} / ${t.size} bytes</td>
    <td>${extra}</td>
  </tr>`;
}
