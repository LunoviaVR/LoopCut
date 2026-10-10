/**
 * Synthetic project data for unit tests and the browser mock. Nothing here is real user data.
 */

import { defaultExportSettings, defaultGrade } from "@/domain/defaults";
import type { Project } from "@/ipc/bindings/Project";

/** A 12-second clip at 20 fps with one kept range, 0 s to 6 s, neutral grade and no captions. */
export function sampleProject(overrides: Partial<Project> = {}): Project {
  return {
    schemaVersion: 1,
    name: "Sample loop",
    clips: [
      {
        id: "clip-1",
        path: "C:/Clips/sample.mp4",
        streamIndex: 0,
        segments: [{ start: 0, end: 6 }],
        holds: [],
      },
    ],
    timing: { speed: 1, direction: "forward", crossfade: 0 },
    transform: {
      crop: { x: 0, y: 0, w: 1, h: 1 },
      cropAspect: null,
      rotation: 0,
      flipH: false,
      flipV: false,
    },
    output: {
      fps: { num: 20, den: 1 },
      size: { mode: "width", width: 480 },
      loopCount: 0,
      background: [0, 0, 0, 255],
    },
    grade: defaultGrade(),
    captions: [],
    export: defaultExportSettings(),
    ...overrides,
  };
}
