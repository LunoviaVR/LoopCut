/**
 * The editor store.
 *
 * Two kinds of state live here:
 * - the **document** (`project`), which is undoable: every edit is one history entry, and continuous
 *   gestures (slider or handle drags) are wrapped in `beginGesture()` / `endGesture()` so that the whole
 *   drag records one entry;
 * - the **ui** (tool, playhead, transport, selection, media registry, dirty flag), which is never
 *   recorded in history.
 */
import { current, original } from "immer";
import { temporal } from "zundo";
import { create, useStore } from "zustand";
import { immer } from "zustand/middleware/immer";
import { clamp, clampInt, isFiniteNumber } from "@/domain/numeric";
import { isValidRational, MAX_OUTPUT_FPS } from "@/domain/time";
import { clampSegmentRange, MIN_SEGMENT_SECONDS, segmentIndexAt } from "@/domain/timeline";
import type { Caption } from "@/ipc/bindings/Caption";
import type { Clip } from "@/ipc/bindings/Clip";
import type { Direction } from "@/ipc/bindings/Direction";
import type { ExportSettings } from "@/ipc/bindings/ExportSettings";
import type { Grade } from "@/ipc/bindings/Grade";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { NormRect } from "@/ipc/bindings/NormRect";
import type { OutputSize } from "@/ipc/bindings/OutputSize";
import type { Project } from "@/ipc/bindings/Project";
import type { Rational } from "@/ipc/bindings/Rational";
import type { SequencePlan } from "@/ipc/bindings/SequencePlan";

/** Undo history depth, in entries. */
export const HISTORY_LIMIT = 200;
export const SPEED_MIN = 0.25;
export const SPEED_MAX = 4;
export const ZOOM_MIN = 0.25;
export const ZOOM_MAX = 16;
/** Smallest crop side, as a fraction of the frame. */
const MIN_CROP_SIZE = 0.01;
/** Highest magnitude for a transport play rate (shuttle speeds 1x, 2x, 4x). */
const PLAY_RATE_MAX = 4;
const PLAY_RATE_MIN = 0.25;

export const TOOL_IDS = ["media", "trim", "crop", "color", "captions", "export"] as const;
export type ToolId = (typeof TOOL_IDS)[number];

/** A selected kept range (by index) or caption (by id). */
export type Selection = { kind: "segment"; index: number } | { kind: "caption"; id: string } | null;

/** Playback state. `playRate` is signed: negative plays in reverse, 1 is normal speed. */
export interface Transport {
  isPlaying: boolean;
  playRate: number;
}

/** A caption to add. The id is generated when omitted. */
export type NewCaption = Omit<Caption, "id"> & { id?: string };

type ProjectRecipe = (draft: Project) => void;

export interface DocumentState {
  project: Project | null;
}

export interface UiState {
  activeTool: ToolId;
  /** Output frame index under the playhead. */
  playhead: number;
  isPlaying: boolean;
  playRate: number;
  loopPlayback: boolean;
  timelineZoom: number;
  /** Whether the inspector column is shown. Choosing a tool reopens it. */
  inspectorOpen: boolean;
  /** Output frames the preview reports (0 before a preview exists). Playhead bounds use it first. */
  frameCount: number;
  selection: Selection;
  mediaByToken: Record<string, ImportedMedia>;
  /** True when the project has edits that are not saved to its `.loopcut` file. */
  dirty: boolean;
  projectPath: string | null;
  /**
   * True while an export job runs. The export flow (features/export/exportFlow.ts) sets it when a job starts and
   * clears it on done, error, cancel and a start that fails. The import, update and Ctrl+E guards read it.
   */
  isExporting: boolean;
  /** Sequencer output for the current project, used for playhead bounds and source-time lookups. */
  sequencePlan: SequencePlan | null;
  /** Open gestures. Undo is unavailable while greater than 0. */
  gestureDepth: number;
}

export interface DocumentActions {
  /** Replaces the project, clears history and marks it clean. */
  loadProject: (project: Project) => void;
  /** Applies a raw recipe to the project as one undoable edit. */
  updateProject: (recipe: ProjectRecipe) => void;
  /** Sets one kept range; neighbours clamp it. Returns true when the project changed. */
  setSegment: (index: number, start: number, end: number) => boolean;
  /** Trim: replaces every kept range with the single range `[inTime, outTime)`. */
  setTrim: (inTime: number, outTime: number) => boolean;
  /** Cuts the kept range that contains `time`. Returns false when no cut is possible there. */
  splitAt: (time: number) => boolean;
  /** Removes a kept range. The last remaining range cannot be removed. */
  deleteSegment: (index: number) => boolean;
  setSpeed: (speed: number) => void;
  setDirection: (direction: Direction) => void;
  setCrossfade: (seconds: number) => void;
  setCrop: (crop: NormRect) => void;
  setCropAspect: (aspect: Rational | null) => void;
  rotate: (degrees: 90 | -90) => void;
  toggleFlipH: () => void;
  toggleFlipV: () => void;
  setOutputFps: (fps: Rational) => boolean;
  setOutputSize: (size: OutputSize) => boolean;
  setLoopCount: (count: number) => void;
  setExport: (patch: Partial<ExportSettings>) => void;
  setGrade: (patch: Partial<Grade>) => void;
  /** Adds a caption and selects it. Returns its id, or null when no project is open. */
  addCaption: (caption: NewCaption) => string | null;
  updateCaption: (id: string, patch: Partial<Omit<Caption, "id">>) => void;
  removeCaption: (id: string) => void;
}

export interface UiActions {
  setActiveTool: (tool: ToolId) => void;
  setInspectorOpen: (open: boolean) => void;
  /** Moves the playhead, clamped to the preview's frames (or the plan's, when no preview exists). */
  setPlayhead: (frame: number) => void;
  setTransport: (transport: Partial<Transport>) => void;
  setLoopPlayback: (on: boolean) => void;
  setTimelineZoom: (zoom: number) => void;
  /** Records how many frames the preview reports and keeps the playhead inside them. */
  setFrameCount: (count: number) => void;
  setSelection: (selection: Selection) => void;
  registerMedia: (media: ImportedMedia) => void;
  forgetMedia: (token: string) => void;
  setProjectPath: (path: string | null) => void;
  /** Clears the dirty flag after the project file was written. */
  markSaved: () => void;
  setExporting: (on: boolean) => void;
  setSequencePlan: (plan: SequencePlan | null) => void;
}

export type EditorState = DocumentState & UiState & DocumentActions & UiActions;

function initialUiState(): UiState {
  return {
    activeTool: "media",
    playhead: 0,
    isPlaying: false,
    playRate: 1,
    loopPlayback: true,
    timelineZoom: 1,
    inspectorOpen: true,
    frameCount: 0,
    selection: null,
    mediaByToken: {},
    dirty: false,
    projectPath: null,
    isExporting: false,
    sequencePlan: null,
    gestureDepth: 0,
  };
}

/**
 * Project snapshot taken when the outermost gesture began, with the dirty flag it began with. The snapshot is
 * recorded as one history entry when the gesture ends with a changed project.
 */
let gestureBase: { project: Project | null; dirty: boolean } | null = null;

/** The temporal (undo) store that zundo attaches to `useEditorStore`. */
const history = () => useEditorStore.temporal.getState();

/** Object copy with `undefined` entries removed, so partial patches never erase fields. */
function definedOnly<T extends object>(patch: Partial<T>): Partial<T> {
  return Object.fromEntries(
    Object.entries(patch).filter(([, value]) => value !== undefined),
  ) as Partial<T>;
}

/**
 * Whether two project snapshots hold the same content. Identical references (the common case for ui
 * updates) answer at once; the JSON comparison runs only when a document edit produced a new object.
 */
function sameProject(a: Project | null, b: Project | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return JSON.stringify(a) === JSON.stringify(b);
}

function newId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `caption-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function isValidOutputSize(size: OutputSize): boolean {
  switch (size.mode) {
    case "width":
      return Number.isInteger(size.width) && size.width > 0;
    case "fit":
      return (
        Number.isInteger(size.maxWidth) &&
        size.maxWidth > 0 &&
        Number.isInteger(size.maxHeight) &&
        size.maxHeight > 0
      );
    case "scale":
      return isFiniteNumber(size.factor) && size.factor > 0;
  }
}

/** Normalizes a play rate: keeps the sign and the 0.25x-4x magnitude range; zero is not a rate. */
function normalizePlayRate(rate: number): number {
  const sign = rate < 0 ? -1 : 1;
  return sign * clamp(Math.abs(rate), PLAY_RATE_MIN, PLAY_RATE_MAX);
}

/**
 * Largest playhead index: the preview's last frame when a preview exists, else the sequence plan's, else
 * unbounded (no playhead limit before either is known).
 */
function maxPlayhead(state: Pick<UiState, "frameCount" | "sequencePlan">): number {
  if (state.frameCount > 0) return state.frameCount - 1;
  return state.sequencePlan
    ? Math.max(0, state.sequencePlan.frames.length - 1)
    : Number.POSITIVE_INFINITY;
}

function selectionAfterSplit(selection: Selection, index: number): Selection {
  if (selection?.kind !== "segment" || selection.index <= index) return selection;
  return { kind: "segment", index: selection.index + 1 };
}

function selectionAfterDelete(selection: Selection, index: number): Selection {
  if (selection?.kind !== "segment") return selection;
  if (selection.index === index) return null;
  return selection.index > index ? { kind: "segment", index: selection.index - 1 } : selection;
}

function selectionAfterSegmentCount(selection: Selection, count: number): Selection {
  return selection?.kind === "segment" && selection.index >= count ? null : selection;
}

export const useEditorStore = create<EditorState>()(
  temporal(
    immer((set, get) => {
      /**
       * Applies one document edit as a single store update, so the project and its dirty flag change
       * together. A recipe that changes nothing leaves the state untouched: no history entry, no dirty
       * flag, no subscriber notification.
       */
      const editProject = (recipe: ProjectRecipe): boolean => {
        if (!get().project) return false;
        let changed = false;
        set((state) => {
          if (!state.project) return;
          recipe(state.project);
          changed = !sameProject(current(state.project), original(state.project) ?? null);
          if (changed) state.dirty = true;
        });
        return changed;
      };

      const clipOf = (project: Project): Clip | undefined => project.clips[0];

      return {
        ...initialUiState(),
        project: null,

        loadProject: (project) => {
          resetGestureTracking();
          set({
            project,
            dirty: false,
            selection: null,
            playhead: 0,
            isPlaying: false,
            playRate: 1,
          });
          history().clear();
        },

        updateProject: (recipe) => {
          editProject(recipe);
        },

        setSegment: (index, start, end) => {
          if (!Number.isInteger(index) || !isFiniteNumber(start) || !isFiniteNumber(end)) {
            return false;
          }
          return editProject((project) => {
            const segments = clipOf(project)?.segments;
            const current = segments?.[index];
            if (!segments || !current) return;
            const range = clampSegmentRange(segments, index, start, end);
            if (!range) return;
            current.start = range.start;
            current.end = range.end;
          });
        },

        setTrim: (inTime, outTime) => {
          if (!isFiniteNumber(inTime) || !isFiniteNumber(outTime)) return false;
          const start = Math.max(0, inTime);
          const end = Math.max(outTime, start + MIN_SEGMENT_SECONDS);
          const changed = editProject((project) => {
            const clip = clipOf(project);
            if (clip) clip.segments = [{ start, end }];
          });
          if (changed) {
            set({ selection: selectionAfterSegmentCount(get().selection, 1) });
          }
          return changed;
        },

        splitAt: (time) => {
          const clip = get().project?.clips[0];
          if (!clip || !isFiniteNumber(time)) return false;
          const index = segmentIndexAt(clip.segments, time);
          const segment = clip.segments[index];
          if (!segment) return false;
          if (time - segment.start < MIN_SEGMENT_SECONDS) return false;
          if (segment.end - time < MIN_SEGMENT_SECONDS) return false;
          const changed = editProject((project) => {
            clipOf(project)?.segments.splice(
              index,
              1,
              { start: segment.start, end: time },
              { start: time, end: segment.end },
            );
          });
          if (changed) {
            set({ selection: selectionAfterSplit(get().selection, index) });
          }
          return changed;
        },

        deleteSegment: (index) => {
          const segments = get().project?.clips[0]?.segments;
          if (!segments || segments.length <= 1 || !Number.isInteger(index)) return false;
          if (!segments[index]) return false;
          const changed = editProject((project) => {
            clipOf(project)?.segments.splice(index, 1);
          });
          if (changed) {
            set({ selection: selectionAfterDelete(get().selection, index) });
          }
          return changed;
        },

        setSpeed: (speed) => {
          if (!isFiniteNumber(speed)) return;
          editProject((project) => {
            project.timing.speed = clamp(speed, SPEED_MIN, SPEED_MAX);
          });
        },

        setDirection: (direction) => {
          editProject((project) => {
            project.timing.direction = direction;
          });
        },

        setCrossfade: (seconds) => {
          if (!isFiniteNumber(seconds)) return;
          editProject((project) => {
            project.timing.crossfade = Math.max(0, seconds);
          });
        },

        setCrop: (crop) => {
          if (!isFiniteNumber(crop.x) || !isFiniteNumber(crop.y)) return;
          if (!isFiniteNumber(crop.w) || !isFiniteNumber(crop.h)) return;
          const w = clamp(crop.w, MIN_CROP_SIZE, 1);
          const h = clamp(crop.h, MIN_CROP_SIZE, 1);
          const next: NormRect = {
            x: clamp(crop.x, 0, 1 - w),
            y: clamp(crop.y, 0, 1 - h),
            w,
            h,
          };
          editProject((project) => {
            project.transform.crop = next;
          });
        },

        setCropAspect: (aspect) => {
          const next =
            aspect && isValidRational(aspect) ? { num: aspect.num, den: aspect.den } : null;
          editProject((project) => {
            project.transform.cropAspect = next;
          });
        },

        rotate: (degrees) => {
          editProject((project) => {
            const turned = project.transform.rotation + degrees;
            project.transform.rotation = ((turned % 360) + 360) % 360;
          });
        },

        toggleFlipH: () => {
          editProject((project) => {
            project.transform.flipH = !project.transform.flipH;
          });
        },

        toggleFlipV: () => {
          editProject((project) => {
            project.transform.flipV = !project.transform.flipV;
          });
        },

        setOutputFps: (fps) => {
          if (!isValidRational(fps) || fps.num / fps.den > MAX_OUTPUT_FPS) return false;
          return editProject((project) => {
            project.output.fps = { num: fps.num, den: fps.den };
          });
        },

        setOutputSize: (size) => {
          if (!isValidOutputSize(size)) return false;
          return editProject((project) => {
            project.output.size = size;
          });
        },

        setLoopCount: (count) => {
          if (!isFiniteNumber(count)) return;
          const next = clampInt(count, 0, 65_535);
          editProject((project) => {
            project.output.loopCount = next;
          });
        },

        setExport: (patch) => {
          const clean = definedOnly(patch);
          editProject((project) => {
            Object.assign(project.export, clean);
            const settings = project.export;
            settings.quality = clampInt(settings.quality, 1, 100);
            settings.lossyQuality = clampInt(settings.lossyQuality, 1, 100);
            settings.motionQuality = clampInt(settings.motionQuality, 1, 100);
            settings.colors = clampInt(settings.colors, 2, 256);
            const target = settings.targetBytes;
            if (target !== null && (!Number.isFinite(target) || target <= 0)) {
              settings.targetBytes = null;
            }
          });
        },

        setGrade: (patch) => {
          const clean = definedOnly(patch);
          editProject((project) => {
            Object.assign(project.grade, clean);
          });
        },

        addCaption: (input) => {
          if (!get().project) return null;
          const caption: Caption = { ...input, id: input.id ?? newId() };
          editProject((project) => {
            project.captions.push(caption);
          });
          set({ selection: { kind: "caption", id: caption.id } });
          return caption.id;
        },

        updateCaption: (id, patch) => {
          const clean = definedOnly(patch);
          editProject((project) => {
            const caption = project.captions.find((item) => item.id === id);
            if (caption) Object.assign(caption, clean);
          });
        },

        removeCaption: (id) => {
          const changed = editProject((project) => {
            const index = project.captions.findIndex((item) => item.id === id);
            if (index >= 0) project.captions.splice(index, 1);
          });
          const selection = get().selection;
          if (changed && selection?.kind === "caption" && selection.id === id) {
            set({ selection: null });
          }
        },

        setActiveTool: (tool) => {
          // The tool controls live in the inspector, so choosing a tool always shows it.
          set({ activeTool: tool, inspectorOpen: true });
        },

        setInspectorOpen: (open) => {
          set({ inspectorOpen: open });
        },

        setPlayhead: (frame) => {
          if (!isFiniteNumber(frame)) return;
          set({ playhead: clamp(Math.floor(frame), 0, maxPlayhead(get())) });
        },

        setTransport: (transport) => {
          const patch: Partial<Pick<UiState, "isPlaying" | "playRate">> = {};
          if (typeof transport.isPlaying === "boolean") patch.isPlaying = transport.isPlaying;
          if (isFiniteNumber(transport.playRate) && transport.playRate !== 0) {
            patch.playRate = normalizePlayRate(transport.playRate);
          }
          set(patch);
        },

        setLoopPlayback: (on) => {
          set({ loopPlayback: on });
        },

        setTimelineZoom: (zoom) => {
          if (!isFiniteNumber(zoom)) return;
          set({ timelineZoom: clamp(zoom, ZOOM_MIN, ZOOM_MAX) });
        },

        setFrameCount: (count) => {
          const next = isFiniteNumber(count) ? Math.max(0, Math.floor(count)) : 0;
          const playhead = clamp(
            get().playhead,
            0,
            maxPlayhead({ frameCount: next, sequencePlan: get().sequencePlan }),
          );
          set({ frameCount: next, playhead });
        },

        setSelection: (selection) => {
          set({ selection });
        },

        registerMedia: (media) => {
          set({ mediaByToken: { ...get().mediaByToken, [media.token]: media } });
        },

        forgetMedia: (token) => {
          const remaining = Object.fromEntries(
            Object.entries(get().mediaByToken).filter(([key]) => key !== token),
          );
          set({ mediaByToken: remaining });
        },

        setProjectPath: (path) => {
          set({ projectPath: path });
        },

        markSaved: () => {
          set({ dirty: false });
        },

        setExporting: (on) => {
          set({ isExporting: on });
        },

        setSequencePlan: (plan) => {
          const limit = maxPlayhead({ frameCount: get().frameCount, sequencePlan: plan });
          set({ sequencePlan: plan, playhead: clamp(get().playhead, 0, limit) });
        },
      };
    }),
    {
      partialize: (state) => ({ project: state.project }),
      equality: (past, next) => sameProject(past.project, next.project),
      limit: HISTORY_LIMIT,
    },
  ),
);

/** Drops an open gesture without recording it. Tracking resumes so later edits are recorded again. */
function resetGestureTracking(): void {
  gestureBase = null;
  history().resume();
  if (useEditorStore.getState().gestureDepth !== 0) {
    useEditorStore.setState({ gestureDepth: 0 });
  }
}

/**
 * Starts a continuous edit (a slider or handle drag). Every edit until the matching `endGesture()`
 * forms one undo step. Calls nest: only the outermost pair counts.
 */
export function beginGesture(): void {
  const { gestureDepth, project } = useEditorStore.getState();
  if (gestureDepth === 0) {
    gestureBase = { project, dirty: useEditorStore.getState().dirty };
    history().pause();
  }
  useEditorStore.setState({ gestureDepth: gestureDepth + 1 });
}

/**
 * Ends the gesture opened by `beginGesture()`. When the outermost gesture changed the project, one
 * history entry (the project from before the gesture) is recorded.
 */
export function endGesture(): void {
  const { gestureDepth } = useEditorStore.getState();
  if (gestureDepth === 0) return;
  if (gestureDepth === 1) {
    const base = gestureBase;
    gestureBase = null;
    const temporalStore = useEditorStore.temporal;
    temporalStore.getState().resume();
    const latest = useEditorStore.getState().project;
    if (base && !sameProject(base.project, latest)) {
      temporalStore.setState((past) => ({
        pastStates: [
          ...(past.pastStates.length >= HISTORY_LIMIT ? past.pastStates.slice(1) : past.pastStates),
          { project: base.project },
        ],
        futureStates: [],
      }));
    } else if (base && !base.dirty) {
      // A gesture that returned the project to its starting content leaves no edit behind: a clean project stays clean.
      useEditorStore.setState({ dirty: false });
    }
  }
  useEditorStore.setState({ gestureDepth: gestureDepth - 1 });
}

/** Runs `fn` between `beginGesture()` and `endGesture()`, even when it throws. */
export function withGesture<T>(fn: () => T): T {
  beginGesture();
  try {
    return fn();
  } finally {
    endGesture();
  }
}

/** Undoes the last document edit. Ignored while a gesture is open. Returns false when nothing changed. */
export function undo(): boolean {
  const tracker = history();
  if (useEditorStore.getState().gestureDepth > 0 || tracker.pastStates.length === 0) return false;
  // Flag dirty first: the restored project then arrives in a single update that is already dirty.
  useEditorStore.setState({ dirty: true });
  tracker.undo();
  return true;
}

/** Redoes the last undone edit. Ignored while a gesture is open. Returns false when nothing changed. */
export function redo(): boolean {
  const tracker = history();
  if (useEditorStore.getState().gestureDepth > 0 || tracker.futureStates.length === 0) return false;
  useEditorStore.setState({ dirty: true });
  tracker.redo();
  return true;
}

/** Whether `undo()` would change anything right now. */
export function canUndo(): boolean {
  return useEditorStore.getState().gestureDepth === 0 && history().pastStates.length > 0;
}

/** Whether `redo()` would change anything right now. */
export function canRedo(): boolean {
  return useEditorStore.getState().gestureDepth === 0 && history().futureStates.length > 0;
}

/** Restores the initial state: no project, default ui, empty history. Intended for tests and reloads. */
export function resetEditorState(): void {
  resetGestureTracking();
  useEditorStore.setState({ ...initialUiState(), project: null });
  history().clear();
}

const selectProject = (state: EditorState): Project | null => state.project;
const selectClip = (state: EditorState): Clip | null => state.project?.clips[0] ?? null;
const selectActiveTool = (state: EditorState): ToolId => state.activeTool;
const selectGestureDepth = (state: EditorState): number => state.gestureDepth;
const selectPastCount = (state: { pastStates: unknown[] }): number => state.pastStates.length;
const selectFutureCount = (state: { futureStates: unknown[] }): number => state.futureStates.length;

/** The open project, or null. */
export function useProject(): Project | null {
  return useEditorStore(selectProject);
}

/** The single clip the v1 editor works on, or null when no project is open. */
export function useClip(): Clip | null {
  return useEditorStore(selectClip);
}

export function useActiveTool(): ToolId {
  return useEditorStore(selectActiveTool);
}

/** Reactive `canUndo()`: true when there is history and no gesture is open. */
export function useCanUndo(): boolean {
  const pastCount = useStore(useEditorStore.temporal, selectPastCount);
  const gestureDepth = useEditorStore(selectGestureDepth);
  return pastCount > 0 && gestureDepth === 0;
}

/** Reactive `canRedo()`: true when there is redo history and no gesture is open. */
export function useCanRedo(): boolean {
  const futureCount = useStore(useEditorStore.temporal, selectFutureCount);
  const gestureDepth = useEditorStore(selectGestureDepth);
  return futureCount > 0 && gestureDepth === 0;
}
