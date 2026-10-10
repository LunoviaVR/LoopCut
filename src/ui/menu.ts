/**
 * Popup menu classes shared by Select and any other menu: overlay fill, the popover shadow and 4px items.
 * Callers add their own width, such as Select's trigger width.
 */

/** The popup surface. Pass it to a Radix Content with `position`, `sideOffset` and `collisionPadding` set. */
export const menuContentClass =
  "z-50 overflow-hidden rounded-control bg-overlay p-1 shadow-popover";

/**
 * One 28px menu row. The left gutter holds a check mark (Select). The highlighted row, which is where keyboard
 * focus sits, takes the accent wash: the hover fill was about 1.04:1 against the overlay and was not visible.
 * High Contrast drops that wash, so the highlight also gets an outline there.
 */
export const menuItemClass =
  "relative flex h-7 cursor-default items-center rounded-chip py-1 pr-2 pl-7 text-sm text-fg outline-none select-none data-[highlighted]:bg-accent-soft data-[disabled]:opacity-40 forced-colors:data-[highlighted]:outline-2 forced-colors:data-[highlighted]:-outline-offset-2 forced-colors:data-[highlighted]:outline-solid";
