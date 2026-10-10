import { Upload } from "lucide-react";
import { type ReactNode, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { saveProject } from "@/features/project/saveProject";
import type { RecoverySnapshot } from "@/ipc/bindings/RecoverySnapshot";
import { appOpenPathArg, onOpenPath } from "@/ipc/commands";
import { useEditorStore } from "@/state/editor";
import { Button, Dialog, toast } from "@/ui";
import { subscribeFileDrag } from "./dragDrop";
import { errorCodeOf, messageOf } from "./errors";
import { fileNameOf, pathKind } from "./formats";
import { pickOpenPath } from "./native";
import {
  commitSession,
  type LoadOptions,
  loadSource,
  releaseMedia,
  type SessionSource,
  setOpening,
  useMediaSession,
} from "./session";

/** A load that is waiting for the user: a dirty project to replace, or a network share to confirm. */
interface Pending {
  source: SessionSource;
  options: LoadOptions;
}

export interface ImportController {
  /** Opens the native "open" dialog (Ctrl+O and the Open video button). */
  requestOpen: () => void;
  /** Opens a video or a .loopcut file. Asks first when the open project has unsaved changes. */
  openPath: (path: string) => void;
  /** Restores a recovery snapshot. */
  openRecovery: (snapshot: RecoverySnapshot) => void;
  /** The drop overlay and the import dialogs. Render once, anywhere in the tree. */
  layer: ReactNode;
}

/**
 * Everything that brings a file into the editor: the open dialog, drag and drop onto the window, paths
 * handed over by a second instance or the command line, recovery snapshots, and the recovery actions (a
 * network-share confirmation, a replacement guard, Locate for missing media).
 *
 * Each open supersedes the previous one, so a slow import can never replace a newer project.
 */
export function useImportController(): ImportController {
  const { t } = useTranslation();
  const sequence = useRef(0);
  const [dragging, setDragging] = useState(false);
  const [replacing, setReplacing] = useState<Pending | null>(null);
  const [confirmingNetwork, setConfirmingNetwork] = useState<Pending | null>(null);

  // Toast actions and the drop listener are created once; they read the latest callbacks through these refs.
  const actions = useRef({
    requestOpen: () => {},
    locate: (_projectPath: string) => {},
  });
  const openRef = useRef<(path: string) => void>(() => {});
  const startedRef = useRef(false);

  const run = useCallback(
    async (source: SessionSource, options: LoadOptions = {}) => {
      const id = ++sequence.current;
      setOpening(nameOfSource(source));
      try {
        const session = await loadSource(source, options);
        if (id !== sequence.current) {
          // A newer open started while this one loaded: release the media it brought in.
          if (session.media.token !== useMediaSession.getState().media?.token) {
            releaseMedia(session.media.token);
          }
          return;
        }
        commitSession(session);
      } catch (error) {
        if (id !== sequence.current) return;
        const code = errorCodeOf(error);
        if (code === "network_path" && !options.confirmNetwork) {
          setConfirmingNetwork({ source, options });
        } else if (code === "media_missing" && source.kind === "project") {
          const projectPath = source.path;
          toast({
            variant: "warn",
            title: t("errors.mediaMissing"),
            description: t("errors.mediaMissingBody"),
            duration: 20000,
            action: {
              label: t("import.locateVideo"),
              onAction: () => actions.current.locate(projectPath),
            },
          });
        } else {
          // The backend's own messages are shown as they come; the one this module writes is translated here.
          const detail = code === "invalid_project" ? t("import.invalidProject") : messageOf(error);
          toast({
            variant: "error",
            title: t("errors.importFailed"),
            description: `${t("errors.importFailedBody")} (${detail})`,
            duration: 12000,
            action: {
              label: t("actions.chooseAnother"),
              onAction: () => actions.current.requestOpen(),
            },
          });
        }
      } finally {
        // Only the latest open clears the name; a superseded one must not hide the newer open's progress.
        if (id === sequence.current) setOpening(null);
      }
    },
    [t],
  );

  const openPending = useCallback(
    (pending: Pending) => {
      const state = useEditorStore.getState();
      if (state.isExporting) {
        toast({
          variant: "warn",
          title: t("import.busyExporting"),
          description: t("import.busyExportingBody"),
        });
        return;
      }
      // Unsaved work would be lost when the project is replaced, so ask first.
      if (state.project && state.dirty) setReplacing(pending);
      else void run(pending.source, pending.options);
    },
    [run, t],
  );

  const openPath = useCallback(
    (path: string) => {
      const kind = pathKind(path);
      if (kind === "unsupported") {
        toast({
          variant: "warn",
          title: t("import.unsupported"),
          description: t("import.unsupportedBody"),
        });
        return;
      }
      openPending({ source: { kind, path }, options: {} });
    },
    [openPending, t],
  );

  const requestOpen = useCallback(() => {
    pickOpenPath()
      .then((path) => {
        if (path) openPath(path);
      })
      .catch((error: unknown) => {
        toast({ variant: "error", title: t("errors.importFailed"), description: messageOf(error) });
      });
  }, [openPath, t]);

  const locate = useCallback(
    (projectPath: string) => {
      pickOpenPath()
        .then((picked) => {
          if (!picked) return;
          if (pathKind(picked) !== "video") {
            toast({
              variant: "warn",
              title: t("import.unsupported"),
              description: t("import.unsupportedBody"),
            });
            return;
          }
          openPending({
            source: { kind: "project", path: projectPath },
            options: { relinkPath: picked },
          });
        })
        .catch((error: unknown) => {
          toast({
            variant: "error",
            title: t("errors.importFailed"),
            description: messageOf(error),
          });
        });
    },
    [openPending, t],
  );

  const openRecovery = useCallback(
    (snapshot: RecoverySnapshot) =>
      openPending({ source: { kind: "recovery", snapshot }, options: {} }),
    [openPending],
  );

  // The refs follow the latest callbacks once each commit lands. Writing them during render would let a
  // render that never commits leave them pointing at callbacks that never took effect.
  useLayoutEffect(() => {
    actions.current = { requestOpen, locate };
    openRef.current = openPath;
  });

  // File drops, and paths a second instance forwards. Subscribed once; the callbacks reach the latest
  // openPath through openRef.
  useEffect(() => {
    let disposed = false;
    const cleanups: Array<() => void> = [];
    const keep = (subscription: Promise<() => void>) => {
      subscription
        .then((unlisten) => {
          if (disposed) unlisten();
          else cleanups.push(unlisten);
        })
        .catch(() => {});
    };

    keep(
      subscribeFileDrag({
        onDragStart: () => setDragging(true),
        onDragEnd: () => setDragging(false),
        onDrop: (paths) => {
          const supported = paths.filter((path) => pathKind(path) !== "unsupported");
          const first = supported[0];
          if (!first) {
            toast({
              variant: "warn",
              title: t("import.unsupported"),
              description: t("import.unsupportedBody"),
            });
            return;
          }
          if (supported.length > 1) {
            toast({ variant: "info", title: t("import.onlyFirst", { name: fileNameOf(first) }) });
          }
          openRef.current(first);
        },
      }),
    );
    keep(onOpenPath((path) => openRef.current(path)));

    return () => {
      disposed = true;
      for (const cleanup of cleanups) cleanup();
    };
  }, [t]);

  // A path passed on the command line at first launch. The ref keeps React's double mount to one call.
  useEffect(() => {
    if (startedRef.current) return;
    startedRef.current = true;
    appOpenPathArg()
      .then((path) => {
        if (path) openRef.current(path);
      })
      .catch(() => {});
  }, []);

  const resolveReplacing = async (choice: "save" | "discard" | "cancel") => {
    const pending = replacing;
    setReplacing(null);
    if (!pending || choice === "cancel") return;
    if (choice === "save") {
      const outcome = await saveProject();
      if (outcome !== "saved" && outcome !== "unchanged") return;
    }
    void run(pending.source, pending.options);
  };

  const confirmNetwork = () => {
    const pending = confirmingNetwork;
    setConfirmingNetwork(null);
    if (pending) void run(pending.source, { ...pending.options, confirmNetwork: true });
  };

  const layer = (
    <>
      <DropOverlay visible={dragging} />
      <ReplaceDialog
        open={replacing !== null}
        onResolve={(choice) => void resolveReplacing(choice)}
      />
      <NetworkDialog
        open={confirmingNetwork !== null}
        name={confirmingNetwork ? nameOfSource(confirmingNetwork.source) : ""}
        onCancel={() => setConfirmingNetwork(null)}
        onConfirm={confirmNetwork}
      />
    </>
  );

  return { requestOpen, openPath, openRecovery, layer };
}

/** The file name a source refers to, for dialog copy. */
function nameOfSource(source: SessionSource): string {
  return source.kind === "recovery" ? source.snapshot.project.name : fileNameOf(source.path);
}

/** Full-window cue while files are dragged over the window. Pointer events pass through it. */
function DropOverlay({ visible }: { visible: boolean }) {
  const { t } = useTranslation();
  if (!visible) return null;
  return (
    <div
      role="status"
      aria-live="polite"
      className="pointer-events-none fixed inset-0 z-40 flex items-center justify-center bg-app/80 p-6"
    >
      <div className="flex flex-col items-center gap-2 rounded-dialog border border-dashed border-accent bg-overlay px-10 py-8 text-center">
        <Upload size={24} strokeWidth={1.5} aria-hidden="true" className="text-accent" />
        <p className="text-lg font-medium text-fg">{t("import.dropTitle")}</p>
        <p className="text-sm text-fg-muted">{t("import.dropBody")}</p>
      </div>
    </div>
  );
}

type ReplaceChoice = "save" | "discard" | "cancel";

function ReplaceDialog({
  open,
  onResolve,
}: {
  open: boolean;
  onResolve: (choice: ReplaceChoice) => void;
}) {
  const { t } = useTranslation();
  const name = useEditorStore((state) => state.project?.name ?? "");
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) onResolve("cancel");
      }}
      size="sm"
      title={t("import.replaceTitle")}
      description={t("import.replaceBody", { name })}
      footer={
        <>
          <Button variant="danger" onClick={() => onResolve("discard")}>
            {t("import.replaceDiscard")}
          </Button>
          <Button variant="secondary" onClick={() => onResolve("cancel")}>
            {t("import.cancel")}
          </Button>
          <Button variant="primary" onClick={() => onResolve("save")}>
            {t("import.replaceSave")}
          </Button>
        </>
      }
    />
  );
}

function NetworkDialog({
  open,
  name,
  onCancel,
  onConfirm,
}: {
  open: boolean;
  name: string;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) onCancel();
      }}
      size="sm"
      title={t("import.networkTitle")}
      description={t("import.networkBody", { name })}
      footer={
        <>
          <Button variant="secondary" onClick={onCancel}>
            {t("import.cancel")}
          </Button>
          <Button variant="primary" onClick={onConfirm}>
            {t("actions.importAnyway")}
          </Button>
        </>
      }
    />
  );
}
