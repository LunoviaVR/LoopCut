import { useState } from "react";
import { useTranslation } from "react-i18next";
import { formatDuration, formatFps, toFloat } from "@/domain/time";
import { displaySize } from "@/features/crop/cropMath";
import type { Direction } from "@/ipc/bindings/Direction";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { PreviewInfo } from "@/ipc/bindings/PreviewInfo";
import type { Project } from "@/ipc/bindings/Project";
import { useEditorStore, withGesture } from "@/state/editor";
import { useEditGesture } from "@/state/gesture";
import {
  Button,
  Field,
  NumberField,
  Section,
  SegmentedControl,
  Select,
  SliderField,
  selectedChipClass,
} from "@/ui";
import {
  DEFAULT_TIMES,
  derivedHeight,
  FPS_PRESETS,
  fpsChoice,
  fpsHint,
  keptRange,
  LOOP_LIMITS,
  type LoopMode,
  loopModeOf,
  maxCrossfadeSeconds,
  OUTPUT_WIDTH_LIMITS,
  rationalFromFps,
  SPEED_LIMITS,
  SPEED_PRESETS,
  WIDTH_PRESETS,
} from "./trimMath";

export interface TrimPanelProps {
  media: ImportedMedia;
  project: Project;
  /** Backend geometry; its output height is shown when present (it accounts for the crop). */
  preview: PreviewInfo | null;
}

/**
 * Trim tool controls: the kept range (read-only, set on the timeline with I and O); speed, direction and crossfade
 * for the loop's timing; frame rate and width for the output; the loop count. Sliders group a drag into one undo
 * step; every discrete choice is one step of its own.
 */
export function TrimPanel({ media, project, preview }: TrimPanelProps) {
  const { t } = useTranslation();
  const speedEdit = useEditGesture();
  const crossfadeEdit = useEditGesture();
  const fpsEdit = useEditGesture();
  const widthEdit = useEditGesture();
  const [forceCustomFps, setForceCustomFps] = useState(false);
  const store = () => useEditorStore.getState();

  const { timing, output } = project;
  const pingPong = timing.direction === "pingPong";
  const crossfadeMax = maxCrossfadeSeconds(project);
  const choice = fpsChoice(output.fps);
  const customFps = forceCustomFps || choice === "custom";
  const hint = fpsHint(output.fps);
  const display = displaySize(media.info.video, project.transform.rotation);
  const width = output.size.mode === "width" ? output.size.width : (preview?.outputWidth ?? 0);
  const height = preview?.outputHeight ?? derivedHeight(width, display);
  const mode = loopModeOf(output.loopCount);
  const range = keptRange(project);

  const setSpeedNow = (value: number) => withGesture(() => store().setSpeed(value));

  const setFpsPreset = (value: string) => {
    if (value === "custom") {
      setForceCustomFps(true);
      return;
    }
    setForceCustomFps(false);
    const rate = rationalFromFps(Number(value));
    withGesture(() => store().setOutputFps(rate));
  };

  const setWidthNow = (value: number) =>
    withGesture(() => store().setOutputSize({ mode: "width", width: Math.round(value) }));

  const setLoopMode = (next: LoopMode) => {
    const current = store().project?.output.loopCount ?? 0;
    if (next === "forever") store().setLoopCount(0);
    else if (next === "once") store().setLoopCount(1);
    else store().setLoopCount(current >= LOOP_LIMITS.min ? current : DEFAULT_TIMES);
  };

  return (
    <>
      <Section title={t("trim.range")}>
        {range ? (
          <>
            <Field label={t("trim.rangeStart")}>
              <span className="tabular min-w-0 truncate text-sm text-fg">
                {formatDuration(range.start)}
              </span>
            </Field>
            <Field label={t("trim.rangeEnd")}>
              <span className="tabular min-w-0 truncate text-sm text-fg">
                {formatDuration(range.end)}
              </span>
            </Field>
          </>
        ) : null}
        <p className="text-xs text-fg-muted">{t("trim.rangeHint")}</p>
      </Section>

      <Section title={t("trim.timing")}>
        <SliderField
          label={t("trim.speed")}
          value={timing.speed}
          min={SPEED_LIMITS.min}
          max={SPEED_LIMITS.max}
          step={SPEED_LIMITS.step}
          decimals={2}
          unit="×"
          defaultValue={1}
          onChange={(value) => speedEdit.change(() => store().setSpeed(value))}
          onCommit={(value) => speedEdit.commit(() => store().setSpeed(value))}
        />
        <fieldset
          className="m-0 flex min-w-0 items-center gap-1 border-0 p-0 pt-1"
          aria-label={t("trim.speedPresets")}
        >
          {SPEED_PRESETS.map((preset) => (
            <Button
              key={preset}
              variant="secondary"
              size="sm"
              aria-pressed={Math.abs(timing.speed - preset) < 1e-9}
              className={selectedChipClass}
              onClick={() => setSpeedNow(preset)}
            >
              <span className="tabular">{t("trim.speedValue", { value: preset })}</span>
            </Button>
          ))}
        </fieldset>

        <Field label={t("trim.direction")} layout="stacked">
          <SegmentedControl<Direction>
            className="w-full"
            aria-label={t("trim.direction")}
            value={timing.direction}
            onValueChange={(direction) => withGesture(() => store().setDirection(direction))}
            items={[
              { value: "forward", label: t("trim.forward") },
              { value: "reverse", label: t("trim.reverse") },
              { value: "pingPong", label: t("trim.pingPong") },
            ]}
          />
        </Field>

        <SliderField
          label={t("trim.crossfade")}
          value={Math.min(timing.crossfade, crossfadeMax)}
          min={0}
          max={Number(crossfadeMax.toFixed(2))}
          step={0.05}
          decimals={2}
          unit="s"
          defaultValue={0}
          disabled={pingPong}
          onChange={(value) => crossfadeEdit.change(() => store().setCrossfade(value))}
          onCommit={(value) => crossfadeEdit.commit(() => store().setCrossfade(value))}
        />
        <p className="text-xs text-fg-muted">
          {pingPong
            ? t("trim.crossfadePingPong")
            : t("trim.crossfadeHint", { max: crossfadeMax.toFixed(2) })}
        </p>
      </Section>

      <Section title={t("trim.output")}>
        <Field label={t("trim.frameRate")}>
          <Select
            aria-label={t("trim.frameRate")}
            value={customFps ? "custom" : choice}
            onValueChange={setFpsPreset}
            options={[
              ...FPS_PRESETS.map((preset) => ({
                value: String(preset),
                label: t("trim.fpsValue", { value: formatFps(rationalFromFps(preset)) }),
              })),
              { value: "custom", label: t("trim.fpsCustom") },
            ]}
          />
        </Field>
        {customFps ? (
          <NumberField
            label={t("trim.fpsCustomLabel")}
            value={toFloat(output.fps)}
            min={1}
            max={50}
            step={0.01}
            decimals={2}
            unit="fps"
            onChange={(value) => fpsEdit.change(() => store().setOutputFps(rationalFromFps(value)))}
            onCommit={(value) => fpsEdit.commit(() => store().setOutputFps(rationalFromFps(value)))}
          />
        ) : null}
        {!hint.exact ? (
          <p className="text-xs text-fg-muted" role="note">
            {hint.clamped
              ? t("trim.fpsCeiling")
              : t("trim.fpsRounded", { value: hint.effective.toFixed(2) })}
          </p>
        ) : null}

        <NumberField
          label={t("trim.width")}
          value={width}
          min={OUTPUT_WIDTH_LIMITS.min}
          max={OUTPUT_WIDTH_LIMITS.max}
          step={1}
          decimals={0}
          unit="px"
          onChange={(value) => widthEdit.change(() => setWidthNow(value))}
          onCommit={(value) => widthEdit.commit(() => setWidthNow(value))}
        />
        <fieldset
          className="m-0 flex min-w-0 items-center gap-1 border-0 p-0 pt-0.5"
          aria-label={t("trim.widthPresets")}
        >
          {WIDTH_PRESETS.map((preset) => (
            <Button
              key={preset}
              variant="secondary"
              size="sm"
              aria-pressed={width === preset}
              className={selectedChipClass}
              onClick={() => setWidthNow(preset)}
            >
              <span className="tabular">{preset}</span>
            </Button>
          ))}
        </fieldset>
        <Field label={t("trim.height")}>
          <span className="tabular min-w-0 truncate text-sm text-fg">
            {t("trim.heightValue", { value: height })}
          </span>
        </Field>
      </Section>

      <Section title={t("trim.loop")}>
        <Field label={t("trim.loopCount")} layout="stacked">
          <SegmentedControl<LoopMode>
            className="w-full"
            aria-label={t("trim.loopCount")}
            value={mode}
            onValueChange={setLoopMode}
            items={[
              { value: "forever", label: t("trim.loopForever") },
              { value: "once", label: t("trim.loopOnce") },
              { value: "times", label: t("trim.loopTimes") },
            ]}
          />
        </Field>
        {mode === "times" ? (
          <NumberField
            label={t("trim.loopPlays")}
            value={output.loopCount}
            min={LOOP_LIMITS.min}
            max={LOOP_LIMITS.max}
            step={1}
            decimals={0}
            onChange={(value) => store().setLoopCount(value)}
            onCommit={(value) => store().setLoopCount(value)}
          />
        ) : null}
      </Section>
    </>
  );
}
