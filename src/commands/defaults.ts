/**
 * Default editor commands, wired to the store. Shortcuts follow the video-editor convention: Space plays, J/K/L
 * shuttle, I/O mark the kept range, S cuts, and the arrows step frames. Tools are Ctrl+1 to Ctrl+6, as the tool
 * rail shows them.
 *
 * This table is the only owner of these shortcuts (useHotkeys binds them). The playhead, transport, active tool,
 * inspector and timeline zoom live in the editor store; the monitor and the panels read them from there.
 *
 * Actions that need UI or backend work (open, save, export, settings) call handlers the UI layer injects with
 * `setCommandHandlers`. Until then they do nothing.
 */
import { type CommandDef, registerCommands } from "@/commands/registry";
import { toFloat } from "@/domain/time";
import { segmentIndexAt, sourceTimeAtFrame } from "@/domain/timeline";
import { redo, type ToolId, undo, useEditorStore } from "@/state/editor";

export interface CommandHandlers {
  /** Ctrl+O: show the open dialog. */
  open: () => void;
  /** Ctrl+S: write the project file. */
  save: () => void;
  /** Ctrl+E: open the export panel or start an export. */
  export: () => void;
  /** Ctrl+,: show the settings dialog. */
  settings: () => void;
}

const defaultHandlers: CommandHandlers = {
  open: () => {},
  save: () => {},
  export: () => {},
  settings: () => {},
};

let handlers: CommandHandlers = { ...defaultHandlers };

/** Replaces some of the injected handlers. Keys left out keep their current handler. */
export function setCommandHandlers(patch: Partial<CommandHandlers>): void {
  handlers = { ...handlers, ...patch };
}

/** Restores the no-op handlers. Intended for tests. */
export function resetCommandHandlers(): void {
  handlers = { ...defaultHandlers };
}

const MAX_SHUTTLE_RATE = 4;
/** Timeline zoom change per zoom command, multiplicative. */
const ZOOM_STEP = 1.25;

const state = () => useEditorStore.getState();
const hasProject = () => state().project !== null;
/** Frames are known once a preview reports them, or once a sequence plan is loaded. */
const hasFrames = () => state().frameCount > 0 || state().sequencePlan !== null;

function setTransport(next: { isPlaying: boolean; playRate: number }): void {
  state().setTransport(next);
}

/** J and L: play in the given direction. Pressing again while already moving doubles the speed, to 4x. */
function shuttle(direction: 1 | -1): void {
  const { isPlaying, playRate } = state();
  const rate =
    isPlaying && Math.sign(playRate) === direction
      ? Math.min(MAX_SHUTTLE_RATE, Math.abs(playRate) * 2)
      : 1;
  setTransport({ isPlaying: true, playRate: direction * rate });
}

/** Whole-second step in frames at the output rate (at least one frame). */
function framesPerSecond(): number {
  const fps = state().project?.output.fps;
  return fps ? Math.max(1, Math.round(toFloat(fps))) : 1;
}

/**
 * The source time under the playhead and the kept range that contains it, or null when the playhead
 * is not inside a kept range (for example over a cut) or no plan is loaded.
 */
function playheadSource(): { index: number; time: number } | null {
  const { sequencePlan, playhead, project } = state();
  if (!sequencePlan || !project) return null;
  const time = sourceTimeAtFrame(sequencePlan, playhead);
  if (time === null) return null;
  const index = segmentIndexAt(project.clips[0]?.segments ?? [], time);
  return index >= 0 ? { index, time } : null;
}

/** I: the kept range under the playhead starts at the playhead. */
function markIn(): void {
  const hit = playheadSource();
  const segment = hit ? state().project?.clips[0]?.segments[hit.index] : undefined;
  if (!hit || !segment) return;
  state().setSegment(hit.index, hit.time, segment.end);
}

/** O: the kept range under the playhead ends at the playhead. */
function markOut(): void {
  const hit = playheadSource();
  const segment = hit ? state().project?.clips[0]?.segments[hit.index] : undefined;
  if (!hit || !segment) return;
  state().setSegment(hit.index, segment.start, hit.time);
}

/** S: cut the kept range at the playhead. */
function splitAtPlayhead(): void {
  const hit = playheadSource();
  if (hit) state().splitAt(hit.time);
}

/** Index of the selected kept range, or null when no range is selected. */
function selectedSegmentIndex(): number | null {
  const selection = state().selection;
  return selection?.kind === "segment" ? selection.index : null;
}

/**
 * Moves the playhead to a frame and stops playback first, so stepping and jumping during playback land on
 * the frame the user asked for rather than where the stream has run on to.
 */
function stepTo(frame: number): void {
  if (state().isPlaying) setTransport({ isPlaying: false, playRate: 1 });
  state().setPlayhead(frame);
}

/** The last frame index known to the editor, or null when no frames are known yet. */
function lastFrame(): number | null {
  const { frameCount, sequencePlan } = state();
  if (frameCount > 0) return frameCount - 1;
  return sequencePlan ? Math.max(0, sequencePlan.frames.length - 1) : null;
}

const tool = (id: ToolId, keys: string[]): CommandDef => ({
  id: `tool.${id}`,
  labelKey: `commands.tool.${id}`,
  group: "tools",
  defaultKeys: keys,
  run: () => state().setActiveTool(id),
});

/** Every default editor command. Each one reads the store when it runs, so the list can be built once. */
export const editorCommands: readonly CommandDef[] = [
  {
    id: "playback.toggle",
    labelKey: "commands.playback.toggle",
    group: "playback",
    defaultKeys: ["Space"],
    when: hasFrames,
    run: () => setTransport({ isPlaying: !state().isPlaying, playRate: 1 }),
  },
  {
    id: "playback.reverse",
    labelKey: "commands.playback.reverse",
    group: "playback",
    defaultKeys: ["KeyJ"],
    when: hasFrames,
    run: () => shuttle(-1),
  },
  {
    id: "playback.pause",
    labelKey: "commands.playback.pause",
    group: "playback",
    defaultKeys: ["KeyK"],
    when: hasFrames,
    run: () => setTransport({ isPlaying: false, playRate: 1 }),
  },
  {
    id: "playback.forward",
    labelKey: "commands.playback.forward",
    group: "playback",
    defaultKeys: ["KeyL"],
    when: hasFrames,
    run: () => shuttle(1),
  },
  {
    id: "timeline.markIn",
    labelKey: "commands.timeline.markIn",
    group: "timeline",
    defaultKeys: ["KeyI"],
    when: () => playheadSource() !== null,
    run: markIn,
  },
  {
    id: "timeline.markOut",
    labelKey: "commands.timeline.markOut",
    group: "timeline",
    defaultKeys: ["KeyO"],
    when: () => playheadSource() !== null,
    run: markOut,
  },
  {
    id: "timeline.split",
    labelKey: "commands.timeline.split",
    group: "timeline",
    defaultKeys: ["KeyS"],
    when: () => playheadSource() !== null,
    run: splitAtPlayhead,
  },
  {
    id: "timeline.deleteSegment",
    labelKey: "commands.timeline.deleteSegment",
    group: "timeline",
    defaultKeys: ["Delete", "Backspace"],
    // Removes the selected kept range or the selected caption: the selection says which one it is.
    when: () => selectedSegmentIndex() !== null || state().selection?.kind === "caption",
    run: () => {
      const selection = state().selection;
      if (selection?.kind === "segment") state().deleteSegment(selection.index);
      else if (selection?.kind === "caption") state().removeCaption(selection.id);
    },
  },
  {
    id: "timeline.frameBack",
    labelKey: "commands.timeline.frameBack",
    group: "timeline",
    defaultKeys: ["ArrowLeft"],
    repeat: true,
    when: hasFrames,
    run: () => stepTo(state().playhead - 1),
  },
  {
    id: "timeline.frameForward",
    labelKey: "commands.timeline.frameForward",
    group: "timeline",
    defaultKeys: ["ArrowRight"],
    repeat: true,
    when: hasFrames,
    run: () => stepTo(state().playhead + 1),
  },
  {
    id: "timeline.secondBack",
    labelKey: "commands.timeline.secondBack",
    group: "timeline",
    defaultKeys: ["Shift+ArrowLeft"],
    repeat: true,
    when: hasFrames,
    run: () => stepTo(state().playhead - framesPerSecond()),
  },
  {
    id: "timeline.secondForward",
    labelKey: "commands.timeline.secondForward",
    group: "timeline",
    defaultKeys: ["Shift+ArrowRight"],
    repeat: true,
    when: hasFrames,
    run: () => stepTo(state().playhead + framesPerSecond()),
  },
  {
    id: "timeline.start",
    labelKey: "commands.timeline.start",
    group: "timeline",
    defaultKeys: ["Home"],
    when: hasProject,
    run: () => stepTo(0),
  },
  {
    id: "timeline.end",
    labelKey: "commands.timeline.end",
    group: "timeline",
    defaultKeys: ["End"],
    when: () => lastFrame() !== null,
    run: () => {
      const last = lastFrame();
      if (last !== null) stepTo(last);
    },
  },
  {
    id: "timeline.zoomIn",
    labelKey: "commands.timeline.zoomIn",
    group: "timeline",
    defaultKeys: ["$mod+Equal", "$mod+NumpadAdd"],
    when: hasProject,
    run: () => state().setTimelineZoom(state().timelineZoom * ZOOM_STEP),
  },
  {
    id: "timeline.zoomOut",
    labelKey: "commands.timeline.zoomOut",
    group: "timeline",
    defaultKeys: ["$mod+Minus", "$mod+NumpadSubtract"],
    when: hasProject,
    run: () => state().setTimelineZoom(state().timelineZoom / ZOOM_STEP),
  },
  {
    id: "timeline.zoomFit",
    labelKey: "commands.timeline.zoomFit",
    group: "timeline",
    defaultKeys: ["$mod+Digit0"],
    when: hasProject,
    run: () => state().setTimelineZoom(1),
  },
  {
    id: "file.open",
    labelKey: "commands.file.open",
    group: "file",
    defaultKeys: ["$mod+KeyO"],
    run: () => handlers.open(),
  },
  {
    id: "file.save",
    labelKey: "commands.file.save",
    group: "file",
    defaultKeys: ["$mod+KeyS"],
    when: hasProject,
    run: () => handlers.save(),
  },
  {
    id: "file.export",
    labelKey: "commands.file.export",
    group: "file",
    defaultKeys: ["$mod+KeyE"],
    when: () => hasProject() && !state().isExporting,
    run: () => handlers.export(),
  },
  {
    id: "app.settings",
    labelKey: "commands.app.settings",
    group: "file",
    defaultKeys: ["$mod+Comma"],
    run: () => handlers.settings(),
  },
  {
    id: "view.inspector",
    labelKey: "commands.view.inspector",
    group: "tools",
    defaultKeys: ["$mod+Backslash"],
    run: () => state().setInspectorOpen(!state().inspectorOpen),
  },
  {
    id: "edit.undo",
    labelKey: "commands.edit.undo",
    group: "edit",
    defaultKeys: ["$mod+KeyZ"],
    run: () => {
      undo();
    },
  },
  {
    id: "edit.redo",
    labelKey: "commands.edit.redo",
    group: "edit",
    defaultKeys: ["$mod+Shift+KeyZ", "$mod+KeyY"],
    run: () => {
      redo();
    },
  },
  tool("media", ["$mod+Digit1"]),
  tool("trim", ["$mod+Digit2"]),
  tool("crop", ["$mod+Digit3", "Alt+KeyC"]),
  tool("color", ["$mod+Digit4"]),
  tool("captions", ["$mod+Digit5"]),
  tool("export", ["$mod+Digit6"]),
];

/** Registers the default editor commands. Returns a disposer that removes them. */
export function registerDefaultCommands(): () => void {
  return registerCommands(editorCommands);
}
