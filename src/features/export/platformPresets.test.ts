import { beforeEach, describe, expect, it } from "vitest";
import type { PlatformPreset } from "@/ipc/bindings/PlatformPreset";
import { resetEditorState, undo, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { applyPlatformPreset, loadPlatformPresets, resetPlatformPresets } from "./platformPresets";

const discord: PlatformPreset = {
  id: "discord",
  name: "Discord",
  format: "gif",
  maxBytes: 10_000_000,
  maxWidth: null,
  maxHeight: null,
  maxFrames: null,
  maxFps: 50,
  recommendedWidth: 480,
  recommendedFps: 20,
  verified: false,
  checkedOn: "2026-10-10",
  source: null,
  note: "note",
};

beforeEach(() => {
  resetEditorState();
  resetPlatformPresets();
  useEditorStore.getState().loadProject(sampleProject());
});

describe("applying a platform preset", () => {
  it("sets the format, the preset id, the target size, the width and the frame rate together", () => {
    useEditorStore.getState().setExport({ format: "webmVp9" });

    applyPlatformPreset(discord);

    const project = useEditorStore.getState().project;
    expect(project?.export).toMatchObject({
      format: "gif",
      preset: "discord",
      targetBytes: 10_000_000,
    });
    expect(project?.output.size).toEqual({ mode: "width", width: 480 });
    expect(project?.output.fps).toEqual({ num: 20, den: 1 });
  });

  it("is one undo step for the whole preset", () => {
    const before = useEditorStore.getState().project;

    applyPlatformPreset(discord);
    undo();

    expect(useEditorStore.getState().project).toEqual(before);
  });

  it("leaves the project's content as it was when the preset already matches it", () => {
    applyPlatformPreset(discord);
    const applied = useEditorStore.getState().project;

    applyPlatformPreset(discord);

    expect(useEditorStore.getState().project).toEqual(applied);
  });
});

describe("loading the presets", () => {
  it("returns the built-in table and keeps it for the session", async () => {
    const first = await loadPlatformPresets();
    const again = await loadPlatformPresets();

    expect(first.map((item) => item.id)).toEqual(expect.arrayContaining(["discord", "x"]));
    expect(again).toBe(first);
  });
});
