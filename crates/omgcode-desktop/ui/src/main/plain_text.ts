/**
 * Turns a rendered reply back into plain text without Markdown symbols, for the reply's
 * **Copy text** button (ADR-0068). It reads the rendered elements, not the source, so the text
 * matches the screen: bidirectional overrides are already visible markers, and a link keeps its
 * shown destination. Paragraphs, headings, quotes, code blocks, and tables are separated by blank
 * lines. Bullets become "•", ordered items keep their numbers, nested items are indented, and
 * table cells are separated by tabs. Code keeps its text exactly, without fences.
 */
export function plainText(root: Element): string {
  if (!root.classList.contains("md")) {
    return root.textContent ?? "";
  }
  return blocks(root).join("\n\n");
}

function blocks(parent: Element): string[] {
  const out: string[] = [];
  for (const child of Array.from(parent.children)) {
    const block = blockText(child);
    if (block !== null) {
      out.push(block);
    }
  }
  return out;
}

function blockText(node: Element): string | null {
  const tag = node.tagName.toLowerCase();
  if (tag === "hr") {
    return null;
  }
  if (tag === "ul" || tag === "ol") {
    return listText(node, tag === "ol");
  }
  if (tag === "blockquote") {
    return blocks(node).join("\n\n");
  }
  if (node.classList.contains("md-code")) {
    return node.querySelector("pre")?.textContent ?? "";
  }
  if (node.classList.contains("md-table")) {
    return Array.from(node.querySelectorAll("tr"))
      .map((row) =>
        Array.from(row.children)
          .map((cell) => cell.textContent ?? "")
          .join("\t"),
      )
      .join("\n");
  }
  return node.textContent ?? "";
}

function listText(list: Element, ordered: boolean): string {
  const start = Number.parseInt(list.getAttribute("start") ?? "1", 10);
  const items = Array.from(list.children).filter((child) => child.tagName === "LI");
  return items
    .map((item, index) => {
      const marker = ordered ? `${start + index}. ` : "• ";
      const indent = " ".repeat(marker.length);
      // A nested list follows its item's text on the next line; other blocks in one item, such as
      // two paragraphs or a paragraph and a code block, are separated by a blank line as they
      // are at the top level. A blank line stays empty rather than indented.
      const lines = itemText(item).split("\n");
      return (
        marker + lines.map((line, n) => (n === 0 || line === "" ? line : indent + line)).join("\n")
      );
    })
    .join("\n");
}

function itemText(item: Element): string {
  let out = "";
  for (const child of Array.from(item.children)) {
    const block = blockText(child);
    if (block === null) {
      continue;
    }
    if (out !== "") {
      const tag = child.tagName.toLowerCase();
      out += tag === "ul" || tag === "ol" ? "\n" : "\n\n";
    }
    out += block;
  }
  return out;
}
