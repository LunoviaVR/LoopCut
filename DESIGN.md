---
name: LoopCut
description: Dark graphite video-editor chrome with one azure accent, a neutral preview bed and tabular numerals, built from Tailwind 4 tokens for a Tauri desktop app.
colors:
  app: "#141518"
  panel: "#1b1c20"
  raised: "#23252a"
  overlay: "#2c2f35"
  hover: "#2a2c31"
  line: "#2f3238"
  line-strong: "#3b3f46"
  line-control: "#767a83"
  preview: "#0b0b0c"
  fg: "#ececef"
  fg-muted: "#a3a7b0"
  fg-subtle: "#6f737c"
  accent: "#3d7eff"
  accent-hover: "#5a92ff"
  accent-press: "#2f68dc"
  accent-fg: "#ffffff"
  accent-soft: "rgb(61 126 255 / 0.16)"
  ok: "#33c481"
  warn: "#f2b84b"
  danger: "#ff5c5c"
typography:
  headline:
    fontFamily: '"Segoe UI Variable Text", "Segoe UI", Inter, system-ui, sans-serif'
    fontSize: "1rem"
    fontWeight: 500
    lineHeight: 1.4
    letterSpacing: normal
  title:
    fontFamily: '"Segoe UI Variable Text", "Segoe UI", Inter, system-ui, sans-serif'
    fontSize: "0.875rem"
    fontWeight: 600
    lineHeight: 1.4
    letterSpacing: normal
  label:
    fontFamily: '"Segoe UI Variable Text", "Segoe UI", Inter, system-ui, sans-serif'
    fontSize: "0.8125rem"
    fontWeight: 500
    lineHeight: 1.4
    letterSpacing: normal
  body:
    fontFamily: '"Segoe UI Variable Text", "Segoe UI", Inter, system-ui, sans-serif'
    fontSize: "0.8125rem"
    fontWeight: 400
    lineHeight: 1.4
    letterSpacing: normal
  caption:
    fontFamily: '"Segoe UI Variable Text", "Segoe UI", Inter, system-ui, sans-serif'
    fontSize: "0.75rem"
    fontWeight: 400
    lineHeight: 1.4
    letterSpacing: normal
  micro:
    fontFamily: '"Segoe UI Variable Text", "Segoe UI", Inter, system-ui, sans-serif'
    fontSize: "0.6875rem"
    fontWeight: 500
    lineHeight: 1
    letterSpacing: normal
rounded:
  chip: "4px"
  control: "6px"
  panel: "8px"
  dialog: "10px"
spacing:
  "0.5": "2px"
  "1": "4px"
  "1.5": "6px"
  "2": "8px"
  "2.5": "10px"
  "3": "12px"
  "4": "16px"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.app}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 12px"
  button-primary-hover:
    backgroundColor: "{colors.accent-hover}"
    textColor: "{colors.app}"
  button-primary-active:
    backgroundColor: "{colors.accent-press}"
    textColor: "{colors.accent-fg}"
  button-secondary:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.fg}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 12px"
  button-secondary-hover:
    backgroundColor: "{colors.overlay}"
    textColor: "{colors.fg}"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.fg-muted}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 12px"
  button-ghost-hover:
    backgroundColor: "{colors.hover}"
    textColor: "{colors.fg}"
  button-danger:
    backgroundColor: "{colors.danger}"
    textColor: "{colors.app}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 12px"
  tool-tab:
    backgroundColor: "transparent"
    textColor: "{colors.fg-muted}"
    rounded: "{rounded.control}"
    width: "48px"
    height: "48px"
  tool-tab-selected:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.fg}"
  select-trigger:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.fg}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    height: "28px"
    padding: "0 8px"
  number-field:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.fg}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    height: "28px"
    width: "80px"
  panel:
    backgroundColor: "{colors.panel}"
    rounded: "{rounded.panel}"
  dialog:
    backgroundColor: "{colors.overlay}"
    textColor: "{colors.fg}"
    rounded: "{rounded.dialog}"
    padding: "16px"
  badge-ok:
    textColor: "{colors.ok}"
    typography: "{typography.micro}"
    rounded: "{rounded.control}"
    height: "18px"
    padding: "0 6px"
  badge-warn:
    textColor: "{colors.warn}"
    typography: "{typography.micro}"
    rounded: "{rounded.control}"
    height: "18px"
    padding: "0 6px"
  badge-danger:
    textColor: "{colors.danger}"
    typography: "{typography.micro}"
    rounded: "{rounded.control}"
    height: "18px"
    padding: "0 6px"
---

# Design System: LoopCut

## Overview

**Creative North Star: "The Honest Monitor"**

LoopCut is a pro editor for turning video into GIF loops. The monitor is the largest thing on screen and the only place colour is judged, so everything around it stays quiet: graphite chrome in three fills, 1px hairlines, and one azure accent. The chrome is dense on purpose, with 11 to 14px type, 28px controls and 32px panel headers, because the user works fast and repeatedly.

The personality is serious and fast with a playful edge. The edge lives in the loop mark (two arc strokes, one azure and one in the foreground colour), in the GIF vocabulary (loop, ping-pong, dither) and in the copy. The chrome itself does not decorate.

The system refuses the generic web-dashboard look: no card grids, no marketing chrome and no oversized type, as set in the confirmed editor brief. The app is dark only. The page declares `color-scheme: dark` and no light tokens exist.

Product principles this system serves: the neutral bed keeps preview and export honest (PRODUCT principle 1); tabular sizes and fps show real output values (principle 3); dense rows and presets keep pro depth out of the way (principle 4).

**Key Characteristics:**
- Preview-dominant: the monitor takes the remaining width and the timeline is one compact dock.
- Three graphite fills and one hairline; depth comes from fill steps, not shadows on tiles.
- One accent (azure, #3d7eff) for action, selection, the playhead and focus.
- Green, amber and red appear only as state.
- Tabular numerals for every value that changes: timecode, size, fps and field values.
- Keyboard first: every control has a visible focus ring, and shortcuts appear in tooltips.

## Colors

Graphite neutrals carry all structure. Azure is the only hue in the chrome, and three semantic colours appear only as state.

### Primary
- **Azure** (#3d7eff, `accent`): the four roles and nothing else. Primary actions: top-bar Export, the export run button, Open video in the empty monitor, Recover, Install, Import anyway and the import save action. Selection: the active tool tile, pressed icon buttons, the on-state of switches and segmented items, the active settings tab, selected chips, the selected caption bar, the selected timeline range outline, the check on the chosen Select option and the zoom menu. The playhead: the line and the head. Focus: the 2px focus outline, the accent edge on a focused input or number field, and the text caret. Text on azure is graphite (`app`, 4.9:1), never white.
  - Accent outside those roles, as the code stands today: progress bar fills (every use), slider thumb hover and press, the active point in the curve editor (the curve line itself is foreground), the dashed drop card and its icon, the crop frame and handles, and the arc of the LoopCut mark. Each is a decision to keep. A new surface does not get accent by following them.
- **Azure Hover** (#5a92ff, `accent-hover`): hover on primary buttons and on a slider thumb.
- **Azure Press** (#2f68dc, `accent-press`): pressed primary buttons, where the label turns white (`accent-fg`, 5.1:1 on this fill), and a pressed slider thumb.
- **Azure Wash** (`accent-soft`, azure at 16%, about rgb(32 44 68) over panel): the selected fill of the Canonical Selection look, the menu highlight under the keyboard and the text selection. Foreground text on the wash reads 11.9:1 over panel and 10.7:1 over raised.

### Secondary
Omitted: the project has one accent.

### Tertiary
Omitted.

### Neutral
- **Graphite Base** (#141518, `app`): the window behind the grid, and the base of every scrim (app at 55 to 80 percent).
- **Panel Graphite** (#1b1c20, `panel`): every tile: top bar, tool rail, monitor, timeline dock and inspector.
- **Raised Graphite** (#23252a, `raised`): controls at rest: inputs, number field value boxes, selects, secondary buttons, switch and segmented tracks, slider and progress tracks, empty-state icon tiles, key caps and unselected caption bars.
- **Overlay Graphite** (#2c2f35, `overlay`): floating layers only: dialogs, tooltips, popup menus, toasts, the update banner, the timeline drag readout and the import drop card.
- **Hover Graphite** (#2a2c31, `hover`): hover fill for ghost buttons, tool tiles, section headings and segmented items. It is 1.04:1 on overlay, so vertical tabs, which live on dialogs, hover with a 6 percent foreground tint (`bg-fg/6`) instead.
- **Hairline** (#2f3238, `line`): 1px dividers in panel headers and footers, section and dialog rules, the segmented track border, the secondary button border and key cap borders. Decoration only, 1.3:1 on panel.
- **Control Edge** (#767a83, `line-control`): the resting border of inputs, selects, number field value boxes, switch tracks and colour swatches. 3.6:1 on raised, 4.0:1 on panel and 3.1:1 on overlay (controls inside dialogs), which meets the 3:1 non-text guideline on every surface a control sits on. Hover moves it to muted text; focus moves it to accent.
- **Strong Hairline** (#3b3f46, `line-strong`): hover borders on secondary buttons, unselected caption bar borders, the kept-range outline on the timeline, the dashed empty-monitor card frame and the scrollbar thumb. Decoration: 1.5:1 on raised, 1.6:1 on panel and 1.9:1 on the bed.
- **Preview Bed** (#0b0b0c, `preview`): the monitor bed, the timeline canvas and the poster frame. Never tinted.
- **Foreground** (#ececef, `fg`): titles, values, selected labels and the slider thumb. 14.4:1 on panel, 13.0:1 on raised, 11.4:1 on overlay.
- **Muted Text** (#a3a7b0, `fg-muted`): field labels, hints, meta text, ghost icons at rest, neutral badge text, placeholders, the slider fill and the switch thumb at rest. 7.1:1 on panel, 6.4:1 on raised, 5.6:1 on overlay.
- **Subtle Text** (#6f737c, `fg-subtle`): decoration only: the slider zero tick, the caption bar hover border and the scrollbar thumb hover. 3.2:1 on raised and 3.6:1 on panel. Not for text.

### Semantic (state only)
- **Ready Green** (#33c481, `ok`): an export that fits the chosen limit, the verified-encoder badge, a ready proxy, the saved message.
- **Caution Amber** (#f2b84b, `warn`): an estimate near the limit, the CPU fallback notice, warn toasts and the VFR, HDR and interlaced media badges.
- **Failure Red** (#ff5c5c, `danger`): an estimate over the limit, failed alerts, the destructive button, the NumberField error edge and the over-limit badge.

### Named Rules
**The One Accent Rule.** Azure marks only what can be acted on, what is selected, the playhead and focus. It is never decoration and never a second brand colour. The exceptions in the code are listed under Azure; each one is a decision to keep, not a pattern to copy.

**The Neutral Bed Rule.** The monitor and the timeline canvas stay #0b0b0c with no tint. Overlays may dim the bed with graphite, never colour it. Colour judgement happens on the bed.

## Typography

**Display Font:** none. The chrome has no display face.
**Body Font:** Segoe UI Variable Text (fallback Segoe UI, Inter, system-ui, sans-serif), declared as `--font-sans`. No webfont is bundled, so the stack resolves to the platform sans.
**Label/Mono Font:** none in use. `--font-mono` (Cascadia Mono, Consolas) is declared but no component uses it. Numbers stay in the sans with tabular figures.

**Character:** One quiet, system-native UI family. Hierarchy comes from size and weight over an 11 to 16px range, not from a second typeface.

### Hierarchy
Tailwind size names are remapped in `globals.css`: `text-2xs` is 11px, `text-xs` 12px, `text-sm` 13px, `text-base` 14px, `text-lg` 16px. `text-xl` (20px) is defined but unused.

- **Headline** (500, 16px, 1.4, `text-lg`): the only text above 14px; the drop prompt in the import overlay.
- **Title** (600, 14px, 1.4, `text-base font-semibold`): the tool title in a panel header (the inspector heading, the timeline title) and dialog titles. Empty-state headings are 500: 14px, or 13px in the compact form the monitor uses when its bed is short.
- **Label** (500, 13px, 1.4, `text-sm font-medium`): section headings (the inspector section trigger and the settings group headings), medium buttons, toast titles and compact empty-state titles. Settings tab labels are 400: the settings dialog overrides the medium weight.
- **Value** (600, 13px, 1.4, tabular): the transport timecode, set in semibold so it reads first in the transport row.
- **Body** (400, 13px, 1.4, `text-sm`): values in fields and rows, switch labels, dialog and settings text, and the NumberField value. Nothing is prose, so no measure limit applies; inspector text wraps inside the 320px column.
- **Caption** (400, 12px, 1.4, `text-xs`): field and number field labels, hints under controls, tooltips, segmented items, toast descriptions, the monitor readout, export progress lines, the timeline trim keys and small buttons (500, 12px).
- **Micro** (500, 11px, line-height 1, `text-2xs`): badge text, key caps, tool rail names, timeline ruler labels and the colour wheel readout.

**Tool title versus section heading.** A tool title is 14px semibold in a 32px panel header; a section heading is 13px medium inside the body. The step in size and weight keeps the two levels apart.

### Named Rules
**The Small Chrome Rule.** Editor chrome is set at 11 to 16px. Nothing above 16px appears in the chrome, and weight stops at 600.

**The Tabular Rule.** Any number that changes while editing (timecode, sizes, fps, frame counts, field values, ruler labels) uses tabular figures through the `tabular` class. Number fields right-align their values.

## Layout

One CSS grid in `src/app/Shell.tsx` with 6px padding and 6px gaps around every tile. Rows are a 40px top bar, a flexible monitor row and a fixed 172px timeline row. Columns are a 56px tool rail, a flexible monitor column and a 320px inspector, which is present only when open.

```
+------------------------------ top bar 40px ------------------------------+
| tool rail | monitor (flex) + transport 40px          | inspector 320px     |
|   56px    |------------------------------------------| tool sections       |
|           | timeline dock 172px                      | status row 40px    |
+-----------+------------------------------------------+---------------------+
```

- **Top bar** (40px, full width, `panel`): wordmark, project name, undo, redo, settings, inspector toggle (Ctrl+\) and Export (primary, Ctrl+E). 10px sides, 4px between icon groups. The Export hint names why it is off (no clip, or an export running) and sits below the button.
- **Tool rail** (56px column, a `panel` tile with 8px vertical padding): six 48px tiles with 4px gaps, in this order: Media, Trim, Crop, Color, Captions, Export (Ctrl+1 to Ctrl+6). Each tile holds a 16px icon over an 11px name.
- **Monitor** (flexible `panel` tile): a `preview` bed inset 6px from the top and sides, meeting the transport row below. The 40px transport row holds step back, play (secondary), step forward, the loop toggle (pressed when on), the semibold timecode, the playback rate while shuttling and the zoom menu at the right.
- **Inspector** (320px): the active tool's sections in a scrolling body. Below it, a 40px status row under a hairline on every tool except Export: format, output size, frame rate, the estimate and one fit badge when a limit exists. The size target lives only in the Export tool.
- **Timeline dock** (172px): a 32px panel header (title, a trim keys hint when a clip is open, three 24px zoom buttons), then 8px padding. Beneath: a 64px label gutter, an 8px gap and a 124px canvas made of a 22px ruler, 4px, a 64px clip track, 4px and a 30px caption lane. Then 8px padding.

Responsive behaviour:
- The inspector is open at 1120px and wider. At 1119px and below it starts collapsed and the monitor takes its width. Ctrl+\ toggles it at any width.
- Default window 1440 x 900; minimum 1024 x 640 (`src-tauri/tauri.conf.json`). This is a desktop pointer layout with no phone breakpoint.
- The monitor's empty card switches to the compact EmptyState when the bed is under 420px tall.

Density and rhythm:
- Control heights: 24px (small buttons, small icon buttons, segmented items, timeline zoom buttons), 28px (medium buttons, medium icon buttons, inputs, number field value boxes, selects, the play button), 32px (panel headers, section headings and triggers, inspector field rows, settings tabs, vertical tab rows), 40px (top bar, transport, inspector status row), 48px (tool tiles).
- Spacing steps in use (4px grid): 2px inside segmented tracks and between panel header icon buttons; 4px between tool tiles, inside section bodies and between a slider and its number field; 6px between tiles and inside icon-plus-label buttons; 8px in toolbars and inside the timeline canvas wrapper; 10px top-bar sides; 12px panel and section padding and the label-to-control gap in fields; 16px dialog padding, toast inset and the gap between settings tabs and content.
- Inside a section, rows sit 4px apart with 12px bottom padding. Sections are divided by 1px hairlines.

## Elevation & Depth

Depth is tonal. Tiles sit on the graphite fills and are separated by 6px gutters and by 1px hairlines inside them. Tiles, rows and inline controls cast no shadow. Only layers above the page cast one: tooltips, popup menus, dialogs, toasts, the update banner and the timeline drag readout. Selection rings and knockout rings are drawn with shadow syntax but are outlines, not depth (see below).

### Shadow Vocabulary
These are tokens in `src/styles/globals.css`, used as `shadow-tooltip`, `shadow-popover`, `shadow-toast`, `shadow-overlay` and `shadow-readout`.
- **Tooltip** (`shadow-tooltip`, `0 6px 18px rgb(0 0 0 / 0.45)`): hover and focus hints.
- **Popover** (`shadow-popover`, `0 8px 24px rgb(0 0 0 / 0.45)`): select lists, the monitor zoom menu and the caption controls popover.
- **Toast** (`shadow-toast`, `0 12px 32px rgb(0 0 0 / 0.45)`): toasts and the update banner.
- **Overlay** (`shadow-overlay`, `0 18px 48px rgb(0 0 0 / 0.5)`): modal dialogs over the app scrim.
- **Readout** (`shadow-readout`, `0 4px 12px rgb(0 0 0 / 0.4)`): the timeline drag readout above the trim grips.
- Not elevation, written inline: `inset 0 0 0 1px var(--color-accent)` is the selected ring (see the Canonical Selection Rule); `0 0 0 2px var(--color-panel)` is the slider thumb knockout; `0 0 0 1px var(--color-panel)` is the selected HSL swatch knockout.

### Named Rules
**The Float Rule.** Only layers above the page cast shadows. Tiles, rows and controls separate by fill step and hairline. The selection ring is an inset outline, not depth.

## Shapes

Four radius tokens and two exceptions:
- **Chip, 4px** (`rounded-chip`): key caps, segmented items, menu items and the trim grip.
- **Control, 6px** (`rounded-control`): buttons, tool tiles, inputs, number field boxes, selects, badges, tooltips, popup menus, the monitor bed, the timeline canvas, caption bars, the monitor readout and the poster frame.
- **Panel, 8px** (`rounded-panel`): tiles (top bar, tool rail, monitor, timeline dock, inspector), the empty-state icon tile and the monitor's dashed empty card.
- **Dialog, 10px** (`rounded-dialog`): dialogs, toasts, the update banner and the import drop card.
- **Literal 2px**: the square crop handles only (`rounded-[2px]`).
- **Pill** (full): switch tracks and thumbs, slider tracks and thumbs, progress tracks and the trim grip's notch.

Form language: tiles are flat rectangles with no border. Controls take a raised fill and a 1px control edge (`line-control`) that turns muted on hover and accent on focus. Secondary buttons keep the hairline and move to a strong edge on hover. Dashed lines mark a drop target (the monitor empty card in strong hairline, the drop overlay in accent), a reserved lane (the caption lane) or the selected caption's box over the monitor (accent).

Scroll areas: the scrollbar uses `scrollbar-width: thin` with a strong-hairline thumb and a transparent track, with a 10px WebKit fallback. Scroll containers are the Panel body, the settings content frame, the monitor bed and empty card, and the monitor empty card, which also holds the recent projects list (the list has no scroller of its own).

## Components

Each primitive is quiet, dense and keyboard-complete. State shows as an accent wash or an accent border, never as weight or a second hue. Primitives live in `src/ui` and are imported from `@/ui`.

### Named Rules
**The Canonical Selection Rule.** A selected chip, toggle, tab or tile takes one look: an accent wash, foreground text and a 1px accent inset ring, with the accent icon where the item has one. `selectedFillClass` in `src/ui/selected.ts` is the conditional class and `selectedChipClass` is the same look driven by aria-pressed. Selection never shows as weight or a second hue. The HSL swatch is the one exception: a foreground edge with a 1px panel knockout, so the swatch colour stays visible.

### Buttons
- **Shape:** 6px radius, font 500. Medium is 28px high with 12px sides and a 13px label. Small is 24px high with 8px sides and a 12px label. Icon and label are 6px apart.
- **Primary:** azure fill, graphite label. One per surface: top-bar Export, the export run button, Open video in the empty monitor, Recover, Install in the update banner, Import anyway in the network prompt and the save action in the import save prompt.
- **Secondary:** raised fill, hairline border, foreground label. Hover: overlay fill and strong edge. Pressed: hover fill. Used for Play, Try again, Choose folder, Clear cache, Reset crop, Later and toast actions.
- **Ghost:** no fill, muted label. Hover: hover fill and foreground label. Pressed: raised fill. Used for dialog close, toast dismiss, Skip and Release notes.
- **Danger:** danger fill, graphite label, 90 percent on hover, 80 percent pressed. Only for destructive confirms, placed left of the primary action: the import prompt puts discard left of save, and Clear cache shows a secondary button first and then a danger confirm.
- **States:** colour transitions over 150ms on the ease-out-quick curve. Disabled is 40 percent opacity with no pointer events; the wrapper keeps its tooltip. Loading swaps the leading icon for a 14px spinner, sets `aria-busy` and blocks activation.

### Icon Buttons
- **Shape:** 28px square (24px when small), a 16px icon at 1.5px stroke, ghost by default. The monitor's play button uses the secondary variant.
- **Toggle:** `aria-pressed` shows the Canonical Selection look. Used for the inspector toggle and the loop toggle.
- Every icon button has an `aria-label` and a tooltip, with its shortcut when it has one. The one exception is the monitor zoom trigger, which sets `tooltip={false}`.

### Tool Tiles
- A 56px rail column: a `panel` tile with 8px vertical padding, six 48px tiles with 4px gaps. Each tile holds a 16px icon over an 11px name, 4px apart. Rest: muted icon and name. Hover: hover fill with foreground text. Pressed: raised fill. Selected: the Canonical Selection look with an accent icon; the selected tile keeps its wash on hover.
- Roving tabindex: Tab enters the rail on the active tool, Up and Down move between tools and select them, Home and End jump to the ends. Each tile's tooltip sits to the right and shows the name and shortcut.
- The tiles are `role="tab"` in a tablist named Tools. With the inspector open, each tab controls the inspector tabpanel, and that panel is labelled by the active tab.

### Settings Tabs
- Vertical tabs in a 144px list inside the settings dialog: 32px full-width rows with 2px between them, 13px regular text. The selected tab takes the Canonical Selection look.
- The content frame has one fixed height, `min(32rem, 100vh minus 13rem)`, so the dialog never changes height between tabs; a long section scrolls inside it. There is 16px between the list and the content.
- The horizontal underline style exists in `Tabs` (a 2px accent underline on the active tab) but no screen uses it.

### Segmented Control
- A raised track with a 1px hairline border and 2px inner padding, with 1px gaps. Items are 24px high with a 4px radius, 12px text and an optional 16px icon.
- Rest: muted text. Hover: hover fill with foreground text. Pressed: overlay fill. Selected: the Canonical Selection look, written inline in the component.
- Exactly one value is always set: clicking the selected item keeps it. Used for the trim direction, the loop mode and the export engine.

### Select
- A 28px trigger on raised fill with a 1px control edge, 13px text, 8px sides and a 16px muted chevron. Hover: overlay fill and muted edge. Open: muted edge.
- The popup uses the popover shadow, overlay fill, a 6px radius and 4px padding. It is as wide as the trigger, sits 4px below it and keeps 8px from the window edge. Items are 28px rows with a 4px radius and 13px text, with a 28px left gutter for an accent check on the chosen value. The keyboard-highlighted row takes the accent wash. Disabled items are 40 percent opacity.

### Number Field and Slider Field
- **NumberField:** a 32px row on a 7rem label column with a 12px gap. The label is 12px muted and truncates; dragging it scrubs 4px per step (Shift for ten steps), and double-click resets when wired. The value box is 28px on raised fill with a 1px control edge and a 6px radius, holding a right-aligned tabular 13px value and an optional 12px unit. Widths: 64px (sm), 80px (md, default) and 96px (lg).
- Focus: an accent edge with a 2px accent outline, offset 1px. Enter or blur commits, Escape restores, and arrow keys step by one (Shift by ten). An out-of-range value shows a danger edge and a `role="alert"` message below in 12px danger text, and it is not applied.
- **SliderField:** a NumberField row above a full-width slider, 4px apart. Its value box sits at the end of the value column (`align="end"`), so the number lines up with the right end of the slider; other NumberField rows keep the shared control edge at the start of the column. Both drive the same value, and the slider speaks the number and unit as its value text, for example "1.00 x". One undo step per gesture.
- **Settings:** the number field there keeps its label for assistive technology only and sits at the shared control column of its settings row.

### Slider
- **Horizontal:** a 24px hit height around a 4px track on raised fill. The fill is neutral (muted text), never accent. The fill runs from the minimum; when the range crosses zero it runs from zero, which gives a centre origin for bipolar ranges. When the minimum is below zero and the maximum is above it, a 1px subtle-text tick marks zero.
- **Thumb:** a 12px foreground circle with a 2px panel knockout ring. Hover: accent-hover. Press: accent-press. Focus: a 2px accent outline, offset 2px. Disabled: a muted thumb and 40 percent opacity.
- **Vertical:** 16px wide by default (`w-4`) with a 4px track and a 64px minimum height. The colour wheel's master slider overrides this with `w-6`, so its hit area is 24px.

### Field and Section
- **Field, inline:** a 32px row with the label in a 7rem column (12px muted, truncated), a 12px gap and the control at the shared edge. A hint sits under the control, 12px muted and 4px below it. Use it for read-only facts, selects, switches and colour swatch rows, where the label names the control through `htmlFor`.
- **Field, stacked:** the label sits above a full-width control with a 4px gap, and an optional hint sits below. Used for the trim Direction and Loop count rows.
- **Section:** a 32px trigger with a 16px chevron that turns 90 degrees when open (150ms), a 13px medium title and a hover fill. The body has 12px sides, 4px above the first row and 12px below, with 4px between rows. Sections open by default.
- **SectionHeading:** a non-collapsible 32px heading with a hairline below and no chevron. Used for the settings groups (General, Performance, Storage), with optional actions on the right.

### Panel
- A `panel` tile with an optional 32px header: the title on the left (14px semibold, see Title), 24px icon buttons on the right, and a 1px hairline below. The header has 12px left and 6px right padding.
- The body is the scroll container. It has no padding of its own, so sections supply the 12px sides. With a footer it keeps 12px of bottom padding, and `bodyClassName="pb-0"` drops it. The footer sits under a hairline and does not scroll.
- Used by the inspector (its footer is the status row) and the timeline dock (its header carries the trim keys). The monitor is a bare tile with no header.

### Inspector Status Row and Export Tool
- The status row is a 40px line under a hairline on every tool except Export: format, output size (the output frame dimensions), frame rate (for GIF, the rate after centisecond rounding), the estimate and one fit badge when a limit exists. It uses 12px text and has no select.
- The fit is judged against the target when one is set, and otherwise against the platform preset's upload limit. The target and the platform preset are set only in the Export tool. The Export tool itself has no status row because its own estimate section is there.
- In the Export tool, key-value grids use a 7rem label column with 13px text. An estimate failure shows the error with a secondary Try again button. A failed export shows the stored message and its detail under the failed line.

### Badge and Kbd
- **Badge:** 18px high, 6px radius, 6px sides and 11px medium tabular text. Neutral: raised fill with muted text. Ok, warn and danger: a 15 percent tint of the state colour with that colour as text. The accent variant (solid azure with graphite text) is defined but no screen uses it.
- **Kbd:** 16px key caps, 4px radius, 1px hairline, raised fill, 11px muted text. Keys split on `+`.

### Tooltip
- Overlay fill, 6px radius, 8px sides and 4px top and bottom, 12px text, optional key caps, and the tooltip shadow. It opens after 350ms, sits 6px from the trigger and keeps 8px from the window edge.
- Side: right for the tool tiles; bottom for the top-bar Export hint; top by default. A disabled button needs a wrapper to keep its hint.

### Dialog
- Overlay fill, 10px radius, no border, the overlay shadow, over an app scrim at 70 percent. Widths: 22rem (small, for example the network prompt), 30rem (medium, the release notes dialog) and 40rem (large, settings).
- Header: 16px sides and top, a 14px semibold title, a 13px muted description 4px below and a 24px ghost close button. Body: 16px sides, 12px top, 16px bottom, 13px text. Footer: right-aligned under a hairline, 16px sides and 12px vertical padding, with the safe action last and styled primary.
- Escape and the close button dismiss; focus returns to the trigger.

### Settings
- A large dialog with a five-item vertical tab list and one tab body. Every control saves when it changes, so there is no save button; the description reads "Changes save as you make them."
- Settings rows use an 11rem label column (13px muted label, 12px muted hint), a 16px gap and a 36px minimum height. Controls start at the shared control column, which is where the settings number fields sit.

### Toasts and Update Banner
- **Toasts:** bottom right, 16px inset, 360px wide, 8px apart, overlay fill, 10px radius, 12px padding and the toast shadow. A 16px state icon: info in muted text (not accent), success in ok, warn in warn, error in danger. Title 13px medium, description 12px muted, with an optional small secondary button. Warn and error interrupt screen readers; info and success wait their turn.
- **Update banner:** a non-modal status notice at the bottom left, 16px from the bottom and 64px from the left, 416px wide, on overlay fill with a 10px radius, 12px padding and the toast shadow. It shows the version, a progress bar while installing and a failure line when needed, then Release notes and Skip (ghost), Later (secondary) and Install (primary). Release notes open in a medium dialog as plain text.

### Empty States
- **EmptyState:** a 40px raised icon tile with an 8px radius, a 20px icon, a 14px medium title, a 13px muted body and an optional action. Compact: a 32px tile, a 16px icon, a 13px title and 8px gaps, used where the monitor bed is under 420px tall. The title says what is missing and the body says what to do: the monitor reads "Drop a video or press Ctrl+O", and the timeline reads "No clip on the timeline" with "Open a video to start a clip."
- **Monitor empty card:** a 16px-padded card, at most 24rem wide, with a 1px dashed strong-hairline border and an 8px radius, centred in the bed. It scrolls inside the bed when the bed is short. Recent projects follow as ghost rows; the card is the only scroller.
- **Opening:** while a file is read, the monitor shows its name with a spinner and disables Open video. When a project is open and another file opens, a status overlay covers the bed (app at 80 percent).
- **Failure:** a danger headline, the backend reason in 12px muted text when there is one, and a secondary Try again button.

### Progress and Spinner
- **ProgressBar:** a 4px raised track with a pill radius. The fill is accent in every current use: the media proxy, the decode bar along the bottom of the monitor, export progress, the update install and the performance test. Ok, warn and danger tones exist in the primitive, but no screen uses them today. Determinate width transitions over 200ms and exposes `aria-valuenow`. Indeterminate: a one-third segment slides every 1.3s; reduced motion stops the slide.
- **Spinner:** 14px at 1.5px stroke, rotating, in the current text colour. Inside buttons, or with a status label where the state is announced.

### Timeline (signature)
- One `preview` canvas with a 6px radius and DOM overlays for the trim grips, caption bars and the readout. The canvas paints from the same tokens at run time, with the fallback palette in `draw.ts`.
  - Ruler, 22px at the top: 11px labels with 4px to the right of each tick, and 7px tick marks on the baseline.
  - Clip track, 64px, starting 4px below the ruler: filmstrip thumbnails. Removed areas dim with #0b0b0c at 62 percent and a 10 percent white hatch. Kept ranges have a 1px strong-hairline outline; the selected range has a 2px accent outline.
  - Caption lane, 30px, starting 4px below the clip track: a dashed hairline frame. Caption bars sit 3px inside it, 24px tall, with a 6px radius. Unselected bars are raised with a strong edge; the selected bar takes the Canonical Selection look with a transparent edge.
  - Playhead: a 1.5px accent line with a 10 by 7px accent head at the top. This is the one place accent marks time.
  - In and out grips: each is a focusable `slider` with a 24px hit area over the clip track height. The grip itself is a 6px fg tab with a 2px by 12px bg-app notch, and it turns muted on hover. Arrow keys move one frame of the clip; Shift with an arrow moves one second. Focusing a grip selects its kept range, and Delete or Backspace removes it.
  - The drag readout is a 6px-radius overlay with the readout shadow above the grip while it is dragged.
  - The canvas area takes an inset 2px accent outline on keyboard focus.
- **Dock header:** the title in the panel title style. With a clip open, one line of trim keys (I, O, S, Delete) follows it in 12px muted text, which truncates when the dock is narrow and is hidden from screen readers. The three zoom buttons are 24px and run Ctrl+-, Ctrl+= and Ctrl+0.
- **Label gutter:** 64px, holding the "Clip" and "Captions" track labels at 12px.

### Preview Monitor (signature)
- The bed is `preview` inside a 6px inset, letterboxed to fit by default. The zoom menu offers fit, 50 percent, 100 percent and 200 percent; its trigger is the one icon button without a tooltip.
- A bottom-left readout on an app-at-80-percent pill (6px radius, 12px text in foreground) shows format, output size, fps and frame number, and adds "Reduced quality while playing" during playback.
- The timecode in the transport is 13px semibold tabular foreground.
- Overlays: the crop overlay dims the area outside the crop with app at 55 percent, draws a 1px accent border with 8px square handles inside 24px hit areas, and draws 1px guide lines in foreground at 35 percent. The colour compare overlay draws a 1px foreground divider with a 24px grab area and before and after labels on app at 70 percent. The caption overlay draws a dashed accent frame around the selected caption.
- Drop overlay: full window, app at 80 percent scrim, a dashed accent card (40px by 32px padding) on overlay fill with a 24px accent upload icon, a 16px medium title and 13px body. It does not capture pointer input.

### States
- **Rest:** raised fill for inputs and secondary buttons, with a control edge on inputs; transparent for ghost controls and tiles; muted text.
- **Hover:** one step up. Ghost: hover fill with foreground text. Secondary: overlay fill and strong edge. Primary: accent-hover. Inputs and switches: control edge to muted. Caption bars: edge to subtle text.
- **Pressed:** one step deeper. Secondary: hover fill. Primary: accent-press with a white label.
- **Selected:** the Canonical Selection look, or an accent icon for tool tiles. Toggles, presets, the compare toggle and HSL bands use `aria-pressed` for this.
- **Focus-visible:** a 2px accent outline with 1px offset globally. Sliders, the colour wheel and the curve editor take a 2px offset. The timeline canvas takes an inset outline. Inputs show focus as an accent edge.
- **Disabled:** 40 percent opacity with no pointer events; the wrapper keeps its tooltip.
- **Error:** a danger edge and a danger message under the field, or a danger alert line (`role="alert"`) near the action.
- **Loading:** a spinner in the button, a progress bar in the flow, or a spinner with the status in the monitor.
- **Empty:** EmptyState with an action, or a one-line hint (the inspector reads "Open a video to edit it.").

### Forced Colors
- Windows High Contrast drops fills and box shadows, so the global rules put outlines on the states that matter. Selected, pressed, on and checked elements (`aria-selected`, `aria-pressed`, `data-state` on or checked) get a 2px CanvasText outline inset 2px. Dialogs, listboxes and tooltips get a 1px CanvasText outline. Focus outlines use Highlight.
- Sliders, switch thumbs and menu highlights add their own outlines. Canvases keep their colours.
- The rules are declared in `src/styles/globals.css` and the primitives. No real Windows High Contrast session is recorded for them yet.

### Motion
- One duration for chrome: 150ms on the ease-out-quick curve, `cubic-bezier(0.2, 0.7, 0.2, 1)`, for colour, background and transform changes on buttons, tabs, the switch thumb, slider thumbs and the section chevron. Progress width: 200ms. Indeterminate slide: 1.3s, infinite. Spinner: continuous rotation.
- No entrance animations, page transitions or staggered reveals. Dialogs and toasts appear without an authored transition.
- Reduced motion: `prefers-reduced-motion: reduce` forces transition and animation durations to 0.01ms across the app (`globals.css`). The spinner and indeterminate bar also use `motion-reduce:animate-none`.

## Do's and Don'ts

### Accessibility
- **Contrast** (computed from token values): foreground on panel 14.4:1, on raised 13.0:1 and on overlay 11.4:1; muted on panel 7.1:1, on raised 6.4:1 and on overlay 5.6:1; graphite on azure 4.9:1; white on azure-press 5.1:1; accent text on panel 4.6:1; ok, warn and danger text on panel 7.6, 9.5 and 5.6:1; foreground on the accent wash 11.9:1 over panel.
- **Non-text:** the control edge is 3.2:1 on raised and 3.6:1 on panel, meeting the 3:1 guideline for controls. The accent focus outline is 4.6:1 on panel and 4.1:1 on raised.
- **Known gaps:**
  - The control edge is 2.8:1 on overlay, so an input or switch inside a dialog, such as in settings, relies on its fill and value text as well as its edge.
  - Subtle text is 3.2:1 on raised and 3.6:1 on panel. It is decoration only.
  - Danger badge text sits on its 15 percent tint: 4.2:1 on raised and 3.7:1 on overlay.
  - White on the azure fill at rest is 3.7:1, which is why the label is graphite.
  - The hover fill is 1.04:1 on overlay and 1.22:1 on panel, so hover on dialog and menu surfaces is carried by the text colour change; menu highlights use the accent wash instead.
  - Hairlines (1.3:1 on panel) and strong hairlines (1.5:1 on raised, 1.9:1 on the bed) are decorative dividers, hover borders and the dashed empty-card frame.
  - Curve points select within 9px, an 18px target, below the 24px floor. Every other target listed under Hit areas meets 24px.
- **Keyboard:** every control is reachable. Arrows move within the tool rail, tabs, segmented controls and selects. Enter commits and Escape restores in number fields. Sliders and trim grips respond to arrow keys.
- **Hit areas:** interactive targets are at least 24px in one direction: sliders 24px high, switches 24px (a pseudo-element grows the 16px track), trim grips 24px wide, crop handles 24px, the colour compare divider 24px, timeline zoom buttons 24px, small buttons and icon buttons 24px and segmented items 24px high.
- **Names and status:** icon buttons carry `aria-label`. State text uses `role="status"` or `role="alert"`. Warn and error toasts interrupt.
- **Motion:** reduced motion is honoured globally.

### Do:
- **Do** use the token classes (`bg-panel`, `text-fg-muted`, `border-line`, `border-line-control`, `rounded-control`) defined in `src/styles/globals.css`. Chrome colours appear as hex only in `src/styles/globals.css` and as the fallback palette in `src/features/timeline/draw.ts`, which reads the same tokens at run time. Caption and grade colours are user data.
- **Do** give each surface one primary action, styled azure with a graphite label.
- **Do** show every changing number with tabular figures and real output values, including GIF fps after centisecond rounding.
- **Do** put a tooltip on every icon-only button, with its shortcut where one exists, and a visible name on every text control. The monitor zoom trigger is the one documented exception.
- **Do** show selection with the Canonical Selection look: an accent wash, foreground text and a 1px accent inset ring, or an accent icon.
- **Do** keep the monitor bed and the timeline canvas at #0b0b0c, untinted.
- **Do** reuse the existing primitives in `@/ui` and an existing shadow token for a role before adding a new one.
- **Do** style destructive confirms as danger and place them left of the primary action.
- **Do** fill slider tracks with the neutral muted-text colour, never accent.

### Don't:
- **Don't** tint the preview bed, the timeline canvas or any overlay with a colour.
- **Don't** use `fg-subtle` for text, placeholders or hints. Use `fg-muted`.
- **Don't** put white text on the azure fill at rest (3.7:1). The label is graphite; only the pressed state turns white.
- **Don't** add a second accent, CSS gradients or gradient text. The UI code defines no gradients today.
- **Don't** shadow tiles, rows or inline controls. Shadows are for floating layers only.
- **Don't** set type above 16px in the chrome, or add a second typeface for display.
- **Don't** colour a single border side (for example a left accent bar) on cards, rows or callouts.
- **Don't** use emoji or Unicode symbols as icons. Use Lucide icons at 16px with 1.5px stroke.
- **Don't** use a modal for a message that does not block. The update banner is the model.
- **Don't** add card grids or nested cards to the inspector. Use sections and fields.
- **Don't** add accent to a new surface beyond the four roles. The current exceptions are listed under Azure, and a new one needs its own decision first.
- **Don't** add a light theme one role at a time. The app is dark only today, so a light theme needs every role's light value at once.
