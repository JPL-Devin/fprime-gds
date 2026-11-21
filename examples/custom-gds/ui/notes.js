// The Notes tab: a custom element that lists the operator notes and follows new ones.
class NotesTab extends HTMLElement {
  connectedCallback() {
    const list = document.createElement("ul");
    this.replaceChildren(list);
    this.stop = this.gds.records.subscribe({ kinds: ["example.note"], afterSeq: 0 }, (record) => {
      const item = document.createElement("li");
      item.textContent = `${record.ground_time} ${record.body.text}`;
      list.append(item);
    });
  }
  disconnectedCallback() {
    if (this.stop) this.stop();
  }
}
customElements.define("notes-tab", NotesTab);

export default { id: "notes", title: "Notes", element: "notes-tab" };
