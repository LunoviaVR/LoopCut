import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

// Read from disk: Vitest blanks CSS imports, even with ?raw. Tests run from the repository root.
const globalsCss = readFileSync(resolve(process.cwd(), "src/styles/globals.css"), "utf8");

/** Reads one `--name: value;` declaration from globals.css. */
function token(name: string): string {
  const match = globalsCss.match(new RegExp(`--${name}:\\s*([^;]+);`));
  if (!match?.[1]) throw new Error(`globals.css does not define --${name}`);
  return match[1].trim();
}

/** Relative luminance of a #rrggbb colour (WCAG 2.x). */
function luminance(hex: string): number {
  const [red, green, blue] = [1, 3, 5].map((offset) => {
    const channel = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
    return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * (red ?? 0) + 0.7152 * (green ?? 0) + 0.0722 * (blue ?? 0);
}

function contrast(a: string, b: string): number {
  const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return ((light ?? 0) + 0.05) / ((dark ?? 0) + 0.05);
}

describe("control edges and text contrast", () => {
  it("gives the resting control edge 3:1 against raised, panel and overlay (WCAG 1.4.11)", () => {
    const edge = token("color-line-control");

    expect(contrast(edge, token("color-raised"))).toBeGreaterThanOrEqual(3);
    expect(contrast(edge, token("color-panel"))).toBeGreaterThanOrEqual(3);
    // Settings and other dialogs put inputs and switches on the overlay surface.
    expect(contrast(edge, token("color-overlay"))).toBeGreaterThanOrEqual(3);
  });

  it("keeps placeholder text, which is fg-muted, at 4.5:1 on raised", () => {
    expect(contrast(token("color-fg-muted"), token("color-raised"))).toBeGreaterThanOrEqual(4.5);
  });
});

describe("shared tokens", () => {
  it("defines the elevation tokens the floating primitives use", () => {
    for (const name of ["tooltip", "popover", "toast", "overlay", "readout"]) {
      expect(token(`shadow-${name}`)).toMatch(/rgb\(0 0 0 \/ 0\.\d+\)$/);
    }
  });

  it("defines the 4px chip radius for key caps, menu items and segmented items", () => {
    expect(token("radius-chip")).toBe("4px");
  });

  it("answers forced colors with outlines on selected and pressed states", () => {
    expect(globalsCss).toContain("@media (forced-colors: active)");
    expect(globalsCss).toContain('[aria-pressed="true"]');
    expect(globalsCss).toContain("outline-color: Highlight");
  });

  it("keeps an edge on dialogs, listboxes and tooltips under forced colors", () => {
    expect(globalsCss).toMatch(/\[role="dialog"\],\s*\[role="listbox"\],\s*\[role="tooltip"\]/);
  });
});
