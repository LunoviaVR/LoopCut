/**
 * Loading a source into the editor: a video (a new project), a .loopcut file, or a recovery snapshot. The
 * loaders do the IPC work and return a `LoadedSession`; `commitSession` puts it into the editor store. Nothing
 * here touches the UI, so the dialogs and toasts live in useImportController.
 */
import { create } from "zustand";
import { MIN_SEGMENT_SECONDS } from "@/domain/timeline";
import { forgetProxy, startProxy } from "@/features/media/proxy";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { OpenedProject } from "@/ipc/bindings/OpenedProject";
import type { Project } from "@/ipc/bindings/Project";
import type { RecoverySnapshot } from "@/ipc/bindings/RecoverySnapshot";
import { CommandError, mediaImport, mediaRelease, projectNew, projectOpen } from "@/ipc/commands";
import { useEditorStore } from "@/state/editor";
import { MissingMediaError } from "./errors";

/** Length of the default kept range for a new project (docs/ipc.md: the first 6 s). */
const DEFAULT_KEPT_SECONDS = 6;

/**
 * The media of the open project, and the file a new open is reading. The monitor and the inspector read these.
 * The size estimate is not here: useEstimateSync makes it after every project change.
 */
export interface MediaSession {
  media: ImportedMedia | null;
  /** The name of the file being opened, from the start of the open until it lands or fails. Null when idle. */
  opening: string | null;
}

export const useMediaSession = create<MediaSession>()(() => ({
  media: null,
  opening: null,
}));

export interface LoadOptions {
  /** Set after the user confirmed opening a file on a network share. */
  confirmNetwork?: boolean;
  /** Points a project whose media is missing at a new file (the Locate action). */
  relinkPath?: string;
}

export type SessionSource =
  | { kind: "video"; path: string }
  | { kind: "project"; path: string }
  | { kind: "recovery"; snapshot: RecoverySnapshot };

export interface LoadedSession {
  project: Project;
  media: ImportedMedia;
  /** The `.loopcut` file the project came from, or null for a project made from a video. */
  path: string | null;
  /** True when the project holds work that is not in its file yet (recovered or relinked). */
  unsaved: boolean;
}

export async function loadSource(
  source: SessionSource,
  options: LoadOptions = {},
): Promise<LoadedSession> {
  switch (source.kind) {
    case "video":
      return loadVideo(source.path, options);
    case "project":
      return loadProjectFile(source.path, options);
    case "recovery":
      return loadRecovery(source.snapshot, options);
  }
}

async function loadVideo(path: string, options: LoadOptions): Promise<LoadedSession> {
  const media = await mediaImport(path, options.confirmNetwork);
  try {
    const project = await projectNew(media.token);
    return finishSession(project, media, null, false);
  } catch (error) {
    void mediaRelease(media.token).catch(() => {});
    throw error;
  }
}

/**
 * Opens a `.loopcut` file. The clip's media token comes from `project_open` (`tokens[clip.id]`). A clip with no
 * token is either missing (Locate) or a network share the user has not confirmed yet; a confirmed share is
 * imported now, because the backend holds it back until then.
 */
async function loadProjectFile(path: string, options: LoadOptions): Promise<LoadedSession> {
  const opened = await projectOpen(path);
  const clip = opened.project.clips[0];
  if (!clip) throw invalidProjectError();

  const relinking = options.relinkPath !== undefined;
  if (!relinking && opened.tokens[clip.id] === undefined) {
    if (!networkMediaOf(opened).includes(clip.path)) throw new MissingMediaError(clip.path);
    if (options.confirmNetwork !== true) throw networkConfirmError(clip.path);
  }

  const media = await mediaImport(options.relinkPath ?? clip.path, options.confirmNetwork);
  try {
    // Tokens that project_open minted but the session does not use go back to the backend.
    for (const token of Object.values(opened.tokens)) {
      if (token !== media.token) void mediaRelease(token).catch(() => {});
    }
    const project = relinking ? relinkProject(opened.project, media) : opened.project;
    return finishSession(project, media, opened.path, relinking);
  } catch (error) {
    void mediaRelease(media.token).catch(() => {});
    throw error;
  }
}

async function loadRecovery(
  snapshot: RecoverySnapshot,
  options: LoadOptions,
): Promise<LoadedSession> {
  const clip = snapshot.project.clips[0];
  if (!clip) throw invalidProjectError();
  const media = await mediaImport(clip.path, options.confirmNetwork);
  try {
    return finishSession(snapshot.project, media, snapshot.originalPath, true);
  } catch (error) {
    void mediaRelease(media.token).catch(() => {});
    throw error;
  }
}

/** Network-share clips reported by `project_open`: the subset of missingMedia that needs confirmation. */
function networkMediaOf(opened: OpenedProject): string[] {
  return opened.networkMedia;
}

function invalidProjectError(): CommandError {
  return new CommandError({
    code: "invalid_project",
    message: "This project has no clip.",
  });
}

function networkConfirmError(path: string): CommandError {
  return new CommandError({
    code: "network_path",
    message:
      "This file is on a network share. Confirm to open it: reading it contacts another computer.",
    detail: path,
  });
}

/**
 * Packs an opened project with its media. It does not wait for a size estimate: the editor shows the project at once,
 * and useEstimateSync sizes it from there.
 */
function finishSession(
  project: Project,
  media: ImportedMedia,
  path: string | null,
  unsaved: boolean,
): LoadedSession {
  return { project, media, path, unsaved };
}

/**
 * Points the project's clip at `media` and keeps its kept ranges inside the new duration. A project whose
 * ranges all fall outside the new file gets the default opening range.
 */
export function relinkProject(project: Project, media: ImportedMedia): Project {
  const clip = project.clips[0];
  if (!clip) return project;
  const duration = media.info.duration;
  const kept = clip.segments
    .map((range) => ({
      start: Math.min(range.start, duration),
      end: Math.min(range.end, duration),
    }))
    .filter((range) => range.end - range.start >= MIN_SEGMENT_SECONDS);
  const segments =
    kept.length > 0 ? kept : [{ start: 0, end: Math.min(DEFAULT_KEPT_SECONDS, duration) }];
  const relinked = {
    ...clip,
    path: media.path,
    streamIndex: media.info.video.index,
    segments,
  };
  return { ...project, clips: [relinked, ...project.clips.slice(1)] };
}

/** Releases a media token and drops its proxy state. Errors are ignored: the cache is disposable. */
export function releaseMedia(token: string): void {
  useEditorStore.getState().forgetMedia(token);
  forgetProxy(token);
  void mediaRelease(token).catch(() => {});
}

/** Puts a loaded session into the editor and the media session, releasing the media it replaces. */
export function commitSession(session: LoadedSession): void {
  const previous = useMediaSession.getState().media;
  const editor = useEditorStore.getState();
  editor.loadProject(session.project);
  editor.registerMedia(session.media);
  editor.setProjectPath(session.path);
  if (session.unsaved) useEditorStore.setState({ dirty: true });
  useMediaSession.setState({ media: session.media });
  if (previous && previous.token !== session.media.token) releaseMedia(previous.token);
  if (session.media.needsProxy) startProxy(session.media);
}

/** Sets or clears the name of the file being opened. The monitor shows it while the open runs. */
export function setOpening(name: string | null): void {
  useMediaSession.setState({ opening: name });
}

/** Clears the media session. Intended for tests. */
export function resetMediaSession(): void {
  useMediaSession.setState({ media: null, opening: null });
}
