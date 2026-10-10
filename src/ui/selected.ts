/**
 * The one selected look for chips, toggles and tabs: an accent wash, foreground text and a 1px accent inset
 * ring. Selection never shows as weight or a second hue (DESIGN.md, States).
 */

/** Selected look for a conditional class, e.g. an active rail tab: `active && selectedFillClass`. */
export const selectedFillClass =
  "bg-accent-soft text-fg shadow-[inset_0_0_0_1px_var(--color-accent)]";

/** The same look driven by aria-pressed, for toggle buttons such as IconButton and the compare toggle. */
export const selectedChipClass =
  "aria-pressed:bg-accent-soft aria-pressed:text-fg aria-pressed:shadow-[inset_0_0_0_1px_var(--color-accent)]";
