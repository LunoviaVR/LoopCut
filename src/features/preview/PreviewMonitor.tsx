import {
  Check,
  FolderOpen,
  Pause,
  Play,
  Repeat,
  StepBack,
  StepForward,
  Upload,
  ZoomIn,
} from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { effectiveGifFps, formatFps, formatTimecode, frameToTime } from "@/domain/time";
import { CaptionOverlay } from "@/features/captions/CaptionOverlay";
import { ColorCompareOverlay } from "@/features/color/ColorCompareOverlay";
import { CropOverlay } from "@/features/crop/CropOverlay";
import { EXPORT_FORMATS } from "@/features/export/exportFormat";
import { messageOf } from "@/features/import/errors";
import { useMediaSession } from "@/features/import/session";
import { playheadSourceTime } from "@/features/timeline/mapping";
import type { ExportFormat } from "@/ipc/bindings/ExportFormat";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import type { Project } from "@/ipc/bindings/Project";
import { previewFrame, previewStop } from "@/ipc/commands";
import { cn } from "@/lib/cn";
import { useEditorStore } from "@/state/editor";
import {
  Button,
  EmptyState,
  IconButton,
  menuContentClass,
  menuItemClass,
  ProgressBar,
  Spinner,
} from "@/ui";
import { enqueuePreviewTask } from "./backendQueue";
import { type FrameRequest, LatestFrameLoader } from "./frameLoader";
import { containSize } from "./layout";
import { drawInto, usePlayback } from "./usePlayback";
import { usePreviewSession } from "./usePreviewSession";

export interface MonitorProps {
  /** The open project, or null for the empty state. */
  project: Project | null;
  /** The open media. The crop overlay needs its dimensions and rotation. */
  media: ImportedMedia | null;
  onOpenVideo: () => void;
  /** Shown under the Open video action in the empty state, e.g. the recent projects list. */
  emptyExtra?: ReactNode;
  /**
   * Reports the current preview geometry, or null when there is none. Must be a stable callback: a new one
   * re-sends the project to the preview backend.
   */
  onPreviewChange: (info: PreviewInfo | null) => void;
}

type Zoom = "fit" | number;

/** A frame or playback failure, with the backend's reason when it gave one. */
interface Failure {
  reason: string | null;
}

const ZOOM_OPTIONS: readonly { value: Zoom; labelKey: string }[] = [
  { value: "fit", labelKey: "monitor.zoomFit" },
  { value: 0.5, labelKey: "monitor.zoomHalf" },
  { value: 1, labelKey: "monitor.zoomActual" },
  { value: 2, labelKey: "monitor.zoomDouble" },
];

/** Below this bed height the empty state uses its compact form, so the card fits the monitor. */
const COMPACT_BED_HEIGHT = 420;

/** Store accessor for event handlers and effects, which read the latest state when they run. */
const editor = () => useEditorStore.getState();

/**
 * Preview monitor: the neutral bed, the frame (or the drop zone), the overlay text and the transport bar. The
 * playhead, play state and loop flag live in the store, so the registry commands and this monitor always agree.
 * The frame is drawn by the Rust side's bytes; while paused it is requested through a latest-wins loader, while
 * playing it streams (usePlayback). With the crop tool active it shows the uncropped source frame and the crop
 * overlay on top. The playhead is read in the store subscription, not during render, so playback never re-renders
 * this component.
 */
export function PreviewMonitor({
  project,
  media,
  onOpenVideo,
  emptyExtra,
  onPreviewChange,
}: MonitorProps) {
  const { t } = useTranslation();
  const bedRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const isPlaying = useEditorStore((state) => state.isPlaying);
  const playRate = useEditorStore((state) => state.playRate);
  const loop = useEditorStore((state) => state.loopPlayback);
  const activeTool = useEditorStore((state) => state.activeTool);
  const opening = useMediaSession((state) => state.opening);
  const [bed, setBed] = useState({ width: 0, height: 0 });
  const [zoom, setZoom] = useState<Zoom>("fit");
  const [frameSize, setFrameSize] = useState<{ width: number; height: number } | null>(null);
  const [failure, setFailure] = useState<Failure | null>(null);

  const cropping = activeTool === "crop" && project !== null && media !== null;
  const dpr = window.devicePixelRatio || 1;
  const bedDeviceWidth = Math.round(bed.width * dpr);
  const bedDeviceHeight = Math.round(bed.height * dpr);
  // The same viewport the session gives the backend, so overlays that render frames get the same size.
  const previewViewport = { width: bed.width || 640, height: bed.height || 360, dpr };
  const compactBed = bed.height > 0 && bed.height < COMPACT_BED_HEIGHT;

  const onFailure = useCallback((error: unknown) => setFailure({ reason: messageOf(error) }), []);
  const {
    info,
    failed: sessionFailed,
    reason: sessionReason,
    decodeProgress,
    retry: retrySession,
  } = usePreviewSession(project, bed, media?.token ?? null, onPreviewChange);
  const shownFailed = failure !== null || sessionFailed;
  const shownReason = failure ? failure.reason : sessionReason;
  usePlayback({ info, canvasRef, onFailure });
  // The transport acts on the loaded clip; while another file opens, the bed is covered and the controls rest too.
  const transportIdle = !info || opening !== null;

  // Track the bed size so the preview is rendered at the size it is shown at.
  useEffect(() => {
    const bedElement = bedRef.current;
    if (!bedElement) return;
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect) setBed({ width: Math.round(rect.width), height: Math.round(rect.height) });
    });
    observer.observe(bedElement);
    return () => observer.disconnect();
  }, []);

  // Latest-wins frame requests. A project change drops whatever was pending from the old project.
  const [loader] = useState(
    () =>
      new LatestFrameLoader({
        // Queued with the other preview calls, so a before/after render cannot swap the project under this frame.
        fetch: (request: FrameRequest) =>
          enqueuePreviewTask(() =>
            previewFrame(
              request.frame,
              request.mode,
              request.sourceTime,
              request.maxWidth,
              request.maxHeight,
            ),
          ),
        onFrame: (buffer) => {
          const canvas = canvasRef.current;
          if (!canvas) return;
          const size = drawInto(canvas, buffer);
          setFrameSize((previous) =>
            previous && previous.width === size.width && previous.height === size.height
              ? previous
              : size,
          );
          setFailure((previous) => (previous === null ? previous : null));
        },
        onError: (error) => setFailure({ reason: messageOf(error) }),
      }),
  );
  useEffect(() => {
    loader.start();
    return () => loader.dispose();
  }, [loader]);

  // The request reads the latest inputs through a ref, so the store subscription below never goes stale.
  const requestInputs = useRef({
    info: null as PreviewInfo | null,
    cropping: false,
    playing: false,
    maxWidth: 0,
    maxHeight: 0,
  });
  const shownInfo = useRef<PreviewInfo | null>(null);
  const requestFrame = useCallback(() => {
    const inputs = requestInputs.current;
    const state = editor();
    if (!inputs.info || inputs.playing) return;
    if (!inputs.cropping) {
      loader.request({ frame: state.playhead, mode: "output" });
      return;
    }
    if (inputs.maxWidth <= 0 || inputs.maxHeight <= 0) return;
    loader.request({
      frame: state.playhead,
      mode: "source",
      sourceTime: playheadSourceTime(state.sequencePlan, state.playhead) ?? 0,
      maxWidth: inputs.maxWidth,
      maxHeight: inputs.maxHeight,
    });
  }, [loader]);

  useEffect(() => {
    // A new preview drops the frame still pending from the old one. The reset comes before the request below,
    // so the first request of the new preview is not cancelled by it.
    if (shownInfo.current !== info) {
      shownInfo.current = info;
      loader.reset();
      setFrameSize(null);
      setFailure(null);
    }
    requestInputs.current = {
      info,
      cropping,
      playing: isPlaying,
      maxWidth: bedDeviceWidth,
      maxHeight: bedDeviceHeight,
    };
    requestFrame();
  }, [info, cropping, bedDeviceWidth, bedDeviceHeight, isPlaying, requestFrame, loader]);

  useEffect(() => {
    return useEditorStore.subscribe((state, previous) => {
      if (state.isPlaying) return;
      if (state.playhead !== previous.playhead || state.sequencePlan !== previous.sequencePlan) {
        requestFrame();
      }
    });
  }, [requestFrame]);

  // The crop tool reads the uncropped frame, so it pauses playback and leaves the zoom at fit.
  useEffect(() => {
    if (cropping && editor().isPlaying) editor().setTransport({ isPlaying: false, playRate: 1 });
  }, [cropping]);

  const pausePlayback = () => {
    if (editor().isPlaying) editor().setTransport({ isPlaying: false, playRate: 1 });
  };

  const togglePlay = () => {
    if (!info || cropping) return;
    editor().setTransport({ isPlaying: !editor().isPlaying, playRate: 1 });
  };

  const stepFrame = (delta: number) => {
    if (!info) return;
    pausePlayback();
    editor().setPlayhead(editor().playhead + delta);
  };

  // Clears the failure and asks again: the session is sent once more, and the frame is requested for it.
  const retry = () => {
    setFailure(null);
    retrySession();
    requestFrame();
  };

  // Stop any stream when the monitor unmounts.
  useEffect(() => {
    return () => {
      void previewStop();
    };
  }, []);

  const frameWidth = frameSize?.width ?? info?.renderWidth ?? 0;
  const frameHeight = frameSize?.height ?? info?.renderHeight ?? 0;
  const fitting = zoom === "fit" || cropping || !info;
  let displaySize: { width: number; height: number } | null = null;
  if (fitting) {
    if (frameWidth > 0 && frameHeight > 0) {
      displaySize = containSize(frameWidth, frameHeight, bed.width, bed.height);
    }
  } else if (info) {
    displaySize = { width: info.outputWidth * zoom, height: info.outputHeight * zoom };
  }

  let content: ReactNode = null;
  if (!project) {
    // The card scrolls inside the bed when the bed is shorter than the card, and centres when it fits.
    content = (
      <div className="m-auto flex max-h-full w-full max-w-sm min-w-0 flex-col items-center overflow-y-auto rounded-panel border border-dashed border-line-strong p-4">
        <EmptyState
          compact={compactBed}
          icon={Upload}
          title={opening ? t("import.opening", { name: opening }) : t("monitor.emptyTitle")}
          body={opening ? <Spinner /> : t("monitor.emptyBody")}
          action={
            <Button
              variant="primary"
              size="md"
              disabled={opening !== null}
              onClick={onOpenVideo}
              leadingIcon={<FolderOpen size={16} strokeWidth={1.5} aria-hidden="true" />}
            >
              {t("monitor.openVideo")}
            </Button>
          }
        />
        {emptyExtra ? <div className="w-full pt-4">{emptyExtra}</div> : null}
      </div>
    );
  } else if (shownFailed) {
    content = (
      <div className="m-auto flex max-w-sm min-w-0 flex-col items-center gap-3 px-4 text-center">
        <p role="alert" className="text-sm text-danger">
          {t("monitor.previewFailed")}
        </p>
        {shownReason ? <p className="text-xs break-words text-fg-muted">{shownReason}</p> : null}
        <Button variant="secondary" size="md" onClick={retry}>
          {t("actions.tryAgain")}
        </Button>
      </div>
    );
  } else if (!info) {
    content = (
      <div className="m-auto flex items-center gap-2 text-sm text-fg-muted">
        <Spinner />
        <span>{t("monitor.rendering")}</span>
      </div>
    );
  }

  const showCanvas = project !== null && !shownFailed && info !== null;

  return (
    <main
      aria-label={t("regions.monitor")}
      className="flex h-full min-h-0 min-w-0 flex-col overflow-hidden rounded-panel bg-panel"
    >
      <div
        ref={bedRef}
        className="relative mx-1.5 mt-1.5 flex min-h-0 min-w-0 flex-1 overflow-auto rounded-control bg-preview"
      >
        {content}
        {showCanvas ? (
          <div
            className="relative m-auto shrink-0"
            style={
              displaySize
                ? { width: displaySize.width, height: displaySize.height }
                : { width: "100%", height: "100%" }
            }
          >
            <canvas
              ref={canvasRef}
              role="img"
              aria-label={t("monitor.previewLabel")}
              className={cn("block h-full w-full", !displaySize && "object-contain")}
            />
            {cropping && media && project && frameSize ? (
              <CropOverlay media={media} project={project} />
            ) : null}
            {activeTool === "captions" && project && info ? (
              <CaptionOverlay
                project={project}
                output={{ width: info.outputWidth, height: info.outputHeight }}
              />
            ) : null}
            {activeTool === "color" && project && info ? (
              <ColorCompareOverlay project={project} viewport={previewViewport} />
            ) : null}
          </div>
        ) : null}
        {showCanvas ? (
          <PreviewOverlay
            info={info}
            format={project?.export.format ?? "gif"}
            playing={isPlaying}
            failed={shownFailed}
          />
        ) : null}
        {project && opening ? (
          <div
            role="status"
            className="absolute inset-0 z-10 flex items-center justify-center gap-2 bg-app/80 text-sm text-fg"
          >
            <Spinner />
            <span>{t("import.opening", { name: opening })}</span>
          </div>
        ) : null}
        {info && decodeProgress < 1 ? (
          <div className="pointer-events-none absolute inset-x-0 bottom-0">
            <ProgressBar label={t("preview.decoding")} value={decodeProgress} tone="accent" />
          </div>
        ) : null}
      </div>

      <div className="flex h-10 shrink-0 items-center gap-1 px-2">
        <IconButton
          icon={StepBack}
          size="sm"
          aria-label={t("monitor.previousFrame")}
          shortcut="ArrowLeft"
          disabled={transportIdle}
          onClick={() => stepFrame(-1)}
        />
        <IconButton
          icon={isPlaying ? Pause : Play}
          variant="secondary"
          aria-label={isPlaying ? t("monitor.pause") : t("monitor.play")}
          shortcut="Space"
          disabled={transportIdle || cropping}
          onClick={togglePlay}
        />
        <IconButton
          icon={StepForward}
          size="sm"
          aria-label={t("monitor.nextFrame")}
          shortcut="ArrowRight"
          disabled={transportIdle}
          onClick={() => stepFrame(1)}
        />
        <IconButton
          icon={Repeat}
          size="sm"
          aria-label={t("monitor.loop")}
          pressed={loop}
          disabled={transportIdle}
          onClick={() => editor().setLoopPlayback(!loop)}
        />
        <TransportReadout info={info} rate={project?.output.fps ?? { num: 1, den: 1 }} />
        {playRate !== 1 && isPlaying ? (
          <span className="tabular text-xs text-fg-muted">
            {t("monitor.shuttle", { rate: playRate })}
          </span>
        ) : null}

        <div className="ml-auto shrink-0">
          <DropdownMenu.Root>
            <DropdownMenu.Trigger asChild>
              <IconButton
                icon={ZoomIn}
                size="sm"
                aria-label={t("monitor.zoom")}
                tooltip={false}
                disabled={!info || cropping}
              />
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content
                align="end"
                sideOffset={6}
                collisionPadding={8}
                className={cn(menuContentClass, "min-w-40")}
              >
                <DropdownMenu.RadioGroup
                  value={String(zoom)}
                  onValueChange={(next) => setZoom(next === "fit" ? "fit" : Number(next))}
                >
                  {ZOOM_OPTIONS.map((option) => (
                    <DropdownMenu.RadioItem
                      key={String(option.value)}
                      value={String(option.value)}
                      className={menuItemClass}
                    >
                      <DropdownMenu.ItemIndicator className="absolute left-2 inline-flex">
                        <Check
                          size={16}
                          strokeWidth={1.5}
                          aria-hidden="true"
                          className="text-accent"
                        />
                      </DropdownMenu.ItemIndicator>
                      {t(option.labelKey)}
                    </DropdownMenu.RadioItem>
                  ))}
                </DropdownMenu.RadioGroup>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
        </div>
      </div>
    </main>
  );
}

/**
 * Format label, output size, frame rate and frame number, over the bottom-left of the bed. A GIF shows the rate it
 * plays at after centisecond rounding; a video shows its output rate.
 */
function PreviewOverlay({
  info,
  format,
  playing,
  failed,
}: {
  info: PreviewInfo | null;
  format: ExportFormat;
  playing: boolean;
  failed: boolean;
}) {
  const { t } = useTranslation();
  const playhead = useEditorStore((state) => state.playhead);
  if (!info || failed) return null;
  const shown = Math.min(playhead, Math.max(0, info.frames - 1));
  const formatLabel = t(
    EXPORT_FORMATS.find((item) => item.value === format)?.labelKey ?? "export.format.gif",
  );
  return (
    <div className="pointer-events-none absolute bottom-2 left-2 flex max-w-[calc(100%-1rem)] flex-wrap items-center gap-x-2 gap-y-0.5 rounded-control bg-app/80 px-2 py-1 text-xs text-fg">
      <span>{formatLabel}</span>
      <span className="tabular">
        {t("preview.outputSize", { width: info.outputWidth, height: info.outputHeight })}
      </span>
      <span className="tabular">
        {format === "gif"
          ? t("preview.gifFps", { value: gifFpsRounded(info) })
          : t("preview.videoFps", { value: formatFps(info.fps) })}
      </span>
      <span className="tabular">
        {t("preview.frameOf", { current: shown + 1, total: info.frames })}
      </span>
      {playing ? <span>{t("preview.reducedWhilePlaying")}</span> : null}
    </div>
  );
}

/** The GIF's effective fps after centisecond rounding, to two decimals, for the overlay. */
function gifFpsRounded(info: PreviewInfo): number {
  return Math.round(effectiveGifFps(info.fps) * 100) / 100;
}

/** Timecode of the playhead next to the transport. Its own component, so playback re-renders only this text. */
function TransportReadout({
  info,
  rate,
}: {
  info: PreviewInfo | null;
  rate: { num: number; den: number };
}) {
  const { t } = useTranslation();
  const playhead = useEditorStore((state) => state.playhead);
  const frames = info?.frames ?? 0;
  const shown = Math.min(playhead, Math.max(0, frames - 1));
  return (
    <span className="tabular ml-2 min-w-0 truncate text-sm font-semibold text-fg">
      {formatTimecode(frameToTime(shown, rate), rate)}
      <span className="ml-1 text-fg-muted">
        {t("monitor.timecodeTotal", {
          total: formatTimecode(frameToTime(frames, rate), rate),
        })}
      </span>
    </span>
  );
}
