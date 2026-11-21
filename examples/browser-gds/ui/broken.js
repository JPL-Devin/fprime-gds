// A tab whose element fails as soon as it is created: the page shows an error panel for
// it and every other tab keeps working.
class BrokenTab extends HTMLElement {
  constructor() {
    super();
    throw new Error("the broken example tab fails on purpose");
  }
}
customElements.define("broken-tab", BrokenTab);
export default { element: "broken-tab" };
