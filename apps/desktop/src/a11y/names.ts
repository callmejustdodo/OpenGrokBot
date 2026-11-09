// A small accessible-name checker for tests (NFR-009): finds buttons, links,
// form fields and widget roles that VoiceOver would read without a name. It
// follows the parts of the accessible-name computation the app relies on:
// aria-labelledby, aria-label, an associated <label>, text content, alt and
// title. A placeholder does not count, since it disappears once the field has
// text. It is not a full implementation and errs towards reporting.

const CONTROLS = [
  "button",
  "a[href]",
  "input:not([type=hidden])",
  "textarea",
  "select",
  '[role="button"]',
  '[role="link"]',
  '[role="switch"]',
  '[role="checkbox"]',
  '[role="radio"]',
  '[role="tab"]',
  '[role="menuitem"]',
  '[role="option"]',
  '[role="textbox"]',
  '[role="combobox"]',
].join(",");

function hidden(el: Element): boolean {
  for (let n: Element | null = el; n; n = n.parentElement) {
    if (n.getAttribute("aria-hidden") === "true" || (n as HTMLElement).hidden) return true;
  }
  return false;
}

function textOf(el: Element): string {
  let out = "";
  for (const child of Array.from(el.childNodes)) {
    if (child.nodeType === Node.TEXT_NODE) out += child.textContent ?? "";
    else if (child instanceof Element && child.getAttribute("aria-hidden") !== "true") {
      out +=
        child.getAttribute("aria-label") ?? (child.tagName === "IMG" ? (child.getAttribute("alt") ?? "") : textOf(child));
    }
  }
  return out;
}

/** The name VoiceOver would read for `el`, or "" when it has none. */
export function accessibleName(el: Element): string {
  const doc = el.ownerDocument;
  const labelledby = el.getAttribute("aria-labelledby");
  if (labelledby) {
    const text = labelledby
      .split(/\s+/)
      .map((id) => doc.getElementById(id))
      .filter((n): n is HTMLElement => n !== null)
      .map(textOf)
      .join(" ")
      .trim();
    if (text) return text;
  }
  const label = el.getAttribute("aria-label")?.trim();
  if (label) return label;
  // A <label for> names any labelable element, a Radix Switch's <button> included.
  if (
    el instanceof HTMLInputElement ||
    el instanceof HTMLTextAreaElement ||
    el instanceof HTMLSelectElement ||
    el instanceof HTMLButtonElement
  ) {
    const labels = Array.from(el.labels ?? [])
      .map(textOf)
      .join(" ")
      .trim();
    if (labels) return labels;
  }
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement) {
    if (el instanceof HTMLInputElement && ["button", "submit", "reset"].includes(el.type) && el.value) return el.value;
  } else {
    const text = textOf(el).trim();
    if (text) return text;
  }
  return (el.getAttribute("title") ?? "").trim();
}

/** Every visible control under `root` without an accessible name, as short descriptions. */
export function unnamedControls(root: ParentNode = document): string[] {
  return Array.from(root.querySelectorAll(CONTROLS))
    .filter((el) => !hidden(el) && accessibleName(el) === "")
    .map((el) => el.outerHTML.slice(0, 200));
}
