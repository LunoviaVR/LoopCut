import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { formatBytes } from "@/domain/bytes";
import { containerLabel } from "@/domain/container";
import { formatDuration, formatFps } from "@/domain/time";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import { cn } from "@/lib/cn";
import { Badge, Field, ProgressBar, Section } from "@/ui";
import { codecName, displaySize, hdrFormat, matrixName, rangeKey } from "./mediaInfo";
import { PosterFrame } from "./PosterFrame";
import { useProxyState } from "./proxy";

function Value({
  children,
  numeric = false,
  title,
}: {
  children: ReactNode;
  numeric?: boolean;
  title?: string;
}) {
  return (
    <span title={title} className={cn("min-w-0 truncate text-sm text-fg", numeric && "tabular")}>
      {children}
    </span>
  );
}

/**
 * Source facts for the Media tool. A header names the clip with its poster, duration and resolution. Then come how
 * the file is encoded and how the preview reads it; the fine colour and scan facts sit under Details, closed.
 */
export function MediaPanel({ media }: { media: ImportedMedia }) {
  const { t } = useTranslation();
  const video = media.info.video;
  const size = displaySize(video);
  const fps = formatFps(video.frameRate);
  const hdr = hdrFormat(video.color.transfer);
  const resolution = `${size.width} × ${size.height}`;

  return (
    <>
      <div className="flex min-w-0 items-center gap-3 border-b border-line px-3 py-3">
        <PosterFrame token={media.token} duration={media.info.duration} />
        <div className="flex min-w-0 flex-col gap-0.5">
          <p className="truncate text-sm font-semibold text-fg" title={media.path}>
            {media.fileName}
          </p>
          <p className="tabular truncate text-xs text-fg-muted">
            {t("media.summary", {
              duration: formatDuration(media.info.duration),
              resolution,
            })}
          </p>
        </div>
      </div>

      <Section title={t("media.source")}>
        <Field label={t("media.container")}>
          <Value title={media.info.container}>
            {containerLabel(media.info.container, media.fileName)}
          </Value>
        </Field>
        <Field label={t("media.codec")}>
          <Value>
            {video.profile
              ? `${codecName(video.codec)} (${video.profile})`
              : codecName(video.codec)}
          </Value>
        </Field>
        <Field label={t("media.fileSize")}>
          <Value numeric>{formatBytes(media.info.sizeBytes)}</Value>
        </Field>
        <Field label={t("media.audio")}>
          <Value>{media.info.hasAudio ? t("media.hasAudio") : t("media.noAudio")}</Value>
        </Field>
      </Section>

      <Section title={t("media.video")}>
        <Field label={t("media.resolution")}>
          <div className="flex min-w-0 flex-col">
            <Value numeric>{resolution}</Value>
            {size.rotated ? (
              <span className="text-xs text-fg-muted">
                {t("media.codedSize", {
                  width: video.width,
                  height: video.height,
                  rotation: video.rotation,
                })}
              </span>
            ) : null}
          </div>
        </Field>
        <Field label={t("media.frameRate")}>
          <span className="flex min-w-0 items-center gap-2">
            <Value numeric>{t("media.fpsValue", { value: fps })}</Value>
            {video.variableFrameRate ? <Badge variant="warn">{t("media.vfr")}</Badge> : null}
          </span>
        </Field>
        <Field label={t("media.frames")}>
          <Value numeric>
            {video.frameCount !== null ? String(video.frameCount) : t("media.unknown")}
          </Value>
        </Field>
      </Section>

      <Section title={t("media.playback")}>
        <Field
          label={t("media.preview")}
          hint={media.needsProxy ? t("media.proxyHint") : undefined}
        >
          <ProxyStatus media={media} />
        </Field>
      </Section>

      <Section title={t("media.details")} defaultOpen={false}>
        <Field label={t("media.bitDepth")}>
          <Value numeric>
            {t("media.bitDepthValue", { bits: video.bitDepth, pixelFormat: video.pixFmt })}
          </Value>
        </Field>
        <Field label={t("media.dynamicRange")}>
          {hdr ? (
            <Badge variant="warn">{t("media.hdr", { format: hdr })}</Badge>
          ) : (
            <Value>{t("media.sdr")}</Value>
          )}
        </Field>
        <Field label={t("media.alpha")}>
          {video.hasAlpha ? (
            <Badge variant="neutral">{t("media.hasAlpha")}</Badge>
          ) : (
            <Value>{t("media.no")}</Value>
          )}
        </Field>
        <Field label={t("media.color")}>
          <span className="flex min-w-0 items-center gap-2">
            <Value>
              {t("media.colorValue", {
                matrix: matrixName(video.color.matrix),
                range: t(`media.${rangeKey(video.color.range)}`),
              })}
            </Value>
            {video.color.wideGamut ? <Badge variant="neutral">{t("media.wideGamut")}</Badge> : null}
          </span>
        </Field>
        {video.interlaced ? (
          <Field label={t("media.scan")}>
            <Badge variant="warn">{t("media.interlaced")}</Badge>
          </Field>
        ) : null}
      </Section>
    </>
  );
}

/** Whether the preview plays the original file or a generated proxy, and the proxy's progress. */
function ProxyStatus({ media }: { media: ImportedMedia }) {
  const { t } = useTranslation();
  const proxy = useProxyState(media.token);

  if (!media.needsProxy) return <Value>{t("media.previewOriginal")}</Value>;

  if (proxy.status === "running") {
    return (
      <div className="flex min-w-0 flex-col gap-1">
        <span className="tabular text-xs text-fg-muted">
          {t("media.proxyBuilding", { percent: Math.round(proxy.value * 100) })}
        </span>
        <ProgressBar label={t("media.proxyLabel")} value={proxy.value} />
      </div>
    );
  }
  if (proxy.status === "ready") return <Badge variant="ok">{t("media.proxyReady")}</Badge>;
  if (proxy.status === "failed") {
    return (
      <div className="flex min-w-0 flex-col gap-1">
        <Badge variant="danger">{t("media.proxyFailed")}</Badge>
        <span className="text-xs text-danger">{proxy.message}</span>
      </div>
    );
  }
  return <Value>{t("media.proxyWaiting")}</Value>;
}
