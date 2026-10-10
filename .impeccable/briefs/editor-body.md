# Editor surface

Scope: the main LoopCut editor window (import, preview monitor, timeline, inspector, export, settings,
update dialog). Visitor mode: **Operate**. Audience: gamers/streamers and meme creators clipping fast,
judging results by look and file size.

## Direction contract

THESIS: The video-editor standard at CapCut-desktop craft, preview-dominant: a big neutral monitor
and a compact single-clip timeline do the work; it refuses the generic web-dashboard look (no card
grids, no marketing chrome, no oversized type).

OWN-WORLD: Dark graphite surfaces in three steps (app #141518, panel #1b1c20, raised #23252a) with
1px hairlines (#2f3238); one azure accent (#3d7eff) for primary action, selection, playhead head,
focus; semantic green/amber/red only for state. Rounded panel tiles (8px) separated by 6px gutters;
6px control radius; lucide 16px icons at 1.5px stroke; one UI family (Segoe UI Variable → Inter),
tabular numerals for timecode, sizes and fps.

STORY: The user drops a video, scrubs and marks in/out on the timeline, shapes the loop (speed,
direction, crop, color, captions) while the monitor shows the exact output, watches the live size
estimate against a platform preset, and exports — never wondering what the GIF will look like.

FIRST VIEWPORT: Top bar (40px): wordmark, project name, undo/redo, Export (accent). Left rail (56px)
of tool tabs (Media, Trim — speed/direction/loop/fps, Crop, Color, Captions, Export). Center: preview monitor tile (fills the remaining width,
neutral #0b0b0c bed, transport + timecode + loop toggle under it). Right inspector tile (320px) for
the active tool, export summary (format, size estimate vs preset) pinned at its bottom. Bottom
timeline tile (~210px): ruler, filmstrip track, caption track, in/out handles, zoom. Empty state:
a centered drop zone inside the monitor with "Drop a video or press Ctrl+O".

FORM: Canon (category standard) chosen in the safer re-roll round; craft bar CapCut desktop;
seed key 1ffc61e5.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the
verdict, DESIGN.md, and every shipping raster carrying its provenance
