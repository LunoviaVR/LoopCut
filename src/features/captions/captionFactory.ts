/**
 * New captions: the meme presets (top and bottom text), the plain text box, the default time range a new caption gets,
 * and the SubRip import flow. Each preset is a complete caption (style, anchor, animation), so the editor only changes
 * what the user changes.
 */
import { defaultCaptionAnimation, defaultCaptionStyle } from "@/domain/defaults";
import { toFloat } from "@/domain/time";
import { segmentIndexAt } from "@/domain/timeline";
import type { Caption } from "@/ipc/bindings/Caption";
import type { Project } from "@/ipc/bindings/Project";
import type { TimeRange } from "@/ipc/bindings/TimeRange";
import { captionsImportSrt, pickSrtFile } from "@/ipc/commands";
import type { NewCaption } from "@/state/editor";
import { useEditorStore } from "@/state/editor";

export type CaptionPresetKind = "top" | "bottom" | "text";

/** Anchor (y, fraction of the frame height) for each preset. Text sits in the middle. */
export const PRESET_Y: Record<CaptionPresetKind, number> = {
  top: 0.1,
  bottom: 0.88,
  text: 0.5,
};

/** Length of the range a new caption gets when the playhead is not inside a kept range. */
export const DEFAULT_CAPTION_SECONDS = 3;

/** Font size of the plain text preset, a fraction of the frame height: smaller than the meme text. */
const TEXT_FONT_SIZE = 0.06;

/** A caption to add for a preset: its style and anchor are the preset's, its range and text are given. */
export function captionForPreset(
  kind: CaptionPresetKind,
  range: TimeRange,
  text: string,
): NewCaption {
  const style =
    kind === "text"
      ? { ...defaultCaptionStyle(), fontSize: TEXT_FONT_SIZE, uppercase: false }
      : defaultCaptionStyle();
  return {
    text,
    range: { start: range.start, end: range.end },
    x: 0.5,
    y: PRESET_Y[kind],
    style,
    animation: defaultCaptionAnimation(),
  };
}

/**
 * The range a new caption gets: the kept range under the playhead when there is one, otherwise a few seconds from the
 * playhead's source time, shortened to fit the clip. `playheadSource` is null before a plan exists.
 */
export function defaultCaptionRange(
  segments: readonly TimeRange[],
  playheadSource: number | null,
  duration: number,
): TimeRange {
  if (playheadSource === null) {
    return { start: 0, end: Math.min(DEFAULT_CAPTION_SECONDS, duration) };
  }
  const index = segmentIndexAt(segments, playheadSource);
  const segment = index >= 0 ? segments[index] : undefined;
  if (segment) return { start: segment.start, end: segment.end };
  const start = Math.min(
    Math.max(0, playheadSource),
    Math.max(0, duration - DEFAULT_CAPTION_SECONDS),
  );
  return { start, end: Math.min(start + DEFAULT_CAPTION_SECONDS, duration) };
}

/**
 * Imports a SubRip file as captions, appended to the project as one undo step. Resolves the number of captions
 * added, or null when the dialog was cancelled. Errors (unreadable file, too many cues) are thrown for the caller.
 */
export async function importSrtCaptions(project: Project): Promise<number | null> {
  const path = await pickSrtFile();
  if (!path) return null;
  const captions: Caption[] = await captionsImportSrt(path, toFloat(project.output.fps));
  if (captions.length === 0) return 0;
  useEditorStore.getState().updateProject((draft) => {
    draft.captions.push(...captions);
  });
  return captions.length;
}
