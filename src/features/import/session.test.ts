import "@/ui/test/env";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { sampleMedia } from "@/features/testFixtures";
import {
  CommandError,
  exportEstimate,
  mediaImport,
  mediaRelease,
  projectNew,
  projectOpen,
} from "@/ipc/commands";
import { resetEditorState, useEditorStore } from "@/state/editor";
import { sampleProject } from "@/state/test-fixtures";
import { MissingMediaError } from "./errors";
import {
  commitSession,
  loadSource,
  relinkProject,
  resetMediaSession,
  useMediaSession,
} from "./session";

vi.mock("@/ipc/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/ipc/commands")>();
  return {
    ...actual,
    mediaImport: vi.fn(),
    mediaRelease: vi.fn(async () => {}),
    projectNew: vi.fn(),
    projectOpen: vi.fn(),
    exportEstimate: vi.fn(),
    mediaProxyStart: vi.fn(async () => {}),
  };
});

beforeEach(() => {
  vi.clearAllMocks();
  resetEditorState();
  resetMediaSession();
});

describe("loading a video", () => {
  it("imports the file and makes a project, without sizing it", async () => {
    const media = sampleMedia();
    vi.mocked(mediaImport).mockResolvedValue(media);
    vi.mocked(projectNew).mockResolvedValue(sampleProject({ name: "ranked-ace" }));

    const session = await loadSource({ kind: "video", path: media.path });

    expect(mediaImport).toHaveBeenCalledWith(media.path, undefined);
    expect(projectNew).toHaveBeenCalledWith(media.token);
    expect(session.path).toBeNull();
    expect(session.unsaved).toBe(false);
    expect(session.media).toEqual(media);
  });

  it("passes the network confirmation through to the import", async () => {
    vi.mocked(mediaImport).mockResolvedValue(sampleMedia());
    vi.mocked(projectNew).mockResolvedValue(sampleProject());

    await loadSource(
      { kind: "video", path: "\\\\server\\share\\clip.mp4" },
      { confirmNetwork: true },
    );

    expect(mediaImport).toHaveBeenCalledWith("\\\\server\\share\\clip.mp4", true);
  });

  it("surfaces the network_path error so the caller can ask first", async () => {
    vi.mocked(mediaImport).mockRejectedValue(
      new CommandError({ code: "network_path", message: "Network share" }),
    );

    await expect(
      loadSource({ kind: "video", path: "\\\\server\\share\\clip.mp4" }),
    ).rejects.toMatchObject({
      code: "network_path",
    });
    expect(projectNew).not.toHaveBeenCalled();
  });
});

describe("size estimate", () => {
  it("does not wait for the estimate, which useEstimateSync makes once the project is in the editor", async () => {
    const project = sampleProject();
    vi.mocked(projectNew).mockResolvedValue(project);
    vi.mocked(mediaImport).mockResolvedValue(sampleMedia());
    vi.mocked(exportEstimate).mockReturnValue(new Promise(() => {}));

    const session = await loadSource({ kind: "video", path: "C:\\Clips\\ranked-ace.mp4" });

    expect(session.project).toEqual(project);
    expect(exportEstimate).not.toHaveBeenCalled();
  });
});

describe("loading a project file", () => {
  it("imports the media of the clip recorded in the project", async () => {
    const project = sampleProject();
    const media = sampleMedia({ path: project.clips[0]?.path ?? "" });
    vi.mocked(projectOpen).mockResolvedValue({
      project,
      path: "C:\\Clips\\reaction.loopcut",
      tokens: { "clip-1": media.token },
      migrated: false,
      networkMedia: [],
      missingMedia: [],
    });
    vi.mocked(mediaImport).mockResolvedValue(media);

    const session = await loadSource({ kind: "project", path: "C:\\Clips\\reaction.loopcut" });

    expect(session.path).toBe("C:\\Clips\\reaction.loopcut");
    expect(session.project).toEqual(project);
    expect(session.unsaved).toBe(false);
    expect(session.media.token).toBe(media.token);
    expect(mediaRelease).not.toHaveBeenCalled();
  });

  it("releases the tokens project_open minted that the session does not use", async () => {
    const project = sampleProject();
    const media = sampleMedia({ path: project.clips[0]?.path ?? "" });
    vi.mocked(projectOpen).mockResolvedValue({
      project,
      path: "C:\\Clips\\reaction.loopcut",
      tokens: { "clip-1": "tok-stale", "clip-gone": "tok-extra" },
      migrated: false,
      networkMedia: [],
      missingMedia: [],
    });
    vi.mocked(mediaImport).mockResolvedValue(media);

    await loadSource({ kind: "project", path: "C:\\Clips\\reaction.loopcut" });

    expect(mediaRelease).toHaveBeenCalledWith("tok-stale");
    expect(mediaRelease).toHaveBeenCalledWith("tok-extra");
    expect(mediaRelease).not.toHaveBeenCalledWith(media.token);
  });

  it("asks for confirmation before reading a clip on a network share, then imports it", async () => {
    const base = sampleProject().clips[0];
    if (!base) throw new Error("sample project has no clip");
    const project = sampleProject({
      clips: [{ ...base, path: "\\\\server\\share\\clip.mp4" }],
    });
    const media = sampleMedia({ path: "\\\\server\\share\\clip.mp4" });
    vi.mocked(projectOpen).mockResolvedValue({
      project,
      path: "C:\\Clips\\reaction.loopcut",
      tokens: {},
      migrated: false,
      networkMedia: ["\\\\server\\share\\clip.mp4"],
      missingMedia: ["\\\\server\\share\\clip.mp4"],
    });
    vi.mocked(mediaImport).mockResolvedValue(media);

    const refused = await loadSource({
      kind: "project",
      path: "C:\\Clips\\reaction.loopcut",
    }).catch((error: unknown) => error);
    expect(refused).toMatchObject({ code: "network_path" });
    expect(mediaImport).not.toHaveBeenCalled();

    const session = await loadSource(
      { kind: "project", path: "C:\\Clips\\reaction.loopcut" },
      { confirmNetwork: true },
    );
    expect(mediaImport).toHaveBeenCalledWith("\\\\server\\share\\clip.mp4", true);
    expect(session.media.token).toBe(media.token);
  });

  it("refuses to load when the media is missing, and asks for a relink instead", async () => {
    vi.mocked(projectOpen).mockResolvedValue({
      project: sampleProject(),
      path: "C:\\Clips\\reaction.loopcut",
      tokens: {},
      migrated: false,
      networkMedia: [],
      missingMedia: ["clip-1"],
    });

    const failure = await loadSource({
      kind: "project",
      path: "C:\\Clips\\reaction.loopcut",
    }).catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(MissingMediaError);
    expect(mediaImport).not.toHaveBeenCalled();
  });

  it("relinks a missing clip to the file the user picked and marks the project unsaved", async () => {
    const project = sampleProject();
    const picked = sampleMedia({ path: "D:\\Footage\\ranked-ace.mp4", token: "tok-relink" });
    vi.mocked(projectOpen).mockResolvedValue({
      project,
      path: "C:\\Clips\\reaction.loopcut",
      tokens: {},
      migrated: false,
      networkMedia: [],
      missingMedia: ["clip-1"],
    });
    vi.mocked(mediaImport).mockResolvedValue(picked);

    const session = await loadSource(
      { kind: "project", path: "C:\\Clips\\reaction.loopcut" },
      { relinkPath: "D:\\Footage\\ranked-ace.mp4" },
    );

    expect(mediaImport).toHaveBeenCalledWith("D:\\Footage\\ranked-ace.mp4", undefined);
    expect(session.project.clips[0]?.path).toBe("D:\\Footage\\ranked-ace.mp4");
    expect(session.unsaved).toBe(true);
  });
});

/** The sample project with its clip's kept ranges replaced. */
function projectWithSegments(segments: { start: number; end: number }[]) {
  const project = sampleProject();
  const clip = project.clips[0];
  if (!clip) throw new Error("the sample project has no clip");
  project.clips[0] = { ...clip, segments };
  return project;
}

describe("relinkProject", () => {
  it("keeps kept ranges that fit the new duration and clamps the rest", () => {
    const project = projectWithSegments([
      { start: 0, end: 4 },
      { start: 9, end: 14 },
    ]);
    const relinked = relinkProject(
      project,
      sampleMedia({ info: { ...sampleMedia().info, duration: 10 } }),
    );

    expect(relinked.clips[0]?.segments).toEqual([
      { start: 0, end: 4 },
      { start: 9, end: 10 },
    ]);
  });

  it("falls back to the opening range when no kept range survives", () => {
    const project = projectWithSegments([{ start: 20, end: 30 }]);
    const relinked = relinkProject(
      project,
      sampleMedia({ info: { ...sampleMedia().info, duration: 8 } }),
    );

    expect(relinked.clips[0]?.segments).toEqual([{ start: 0, end: 6 }]);
  });
});

describe("committing a session", () => {
  it("puts the project and media into the editor and releases the media it replaces", async () => {
    const first = sampleMedia({ token: "tok-old" });
    const second = sampleMedia({ token: "tok-new", path: "C:\\Clips\\second.mp4" });
    commitSession({
      project: sampleProject(),
      media: first,
      path: null,
      unsaved: false,
    });

    commitSession({
      project: sampleProject({ name: "second" }),
      media: second,
      path: "C:\\Clips\\second.loopcut",
      unsaved: true,
    });

    const state = useEditorStore.getState();
    expect(state.project?.name).toBe("second");
    expect(state.projectPath).toBe("C:\\Clips\\second.loopcut");
    expect(state.dirty).toBe(true);
    expect(state.mediaByToken["tok-new"]).toEqual(second);
    expect(state.mediaByToken["tok-old"]).toBeUndefined();
    expect(useMediaSession.getState().media?.token).toBe("tok-new");
    expect(mediaRelease).toHaveBeenCalledWith("tok-old");
  });
});
