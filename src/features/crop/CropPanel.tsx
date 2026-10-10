import { FlipHorizontal2, FlipVertical2, RotateCcw, RotateCw } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import type { Project } from "@/ipc/bindings/Project";
import type { Rational } from "@/ipc/bindings/Rational";
import { useEditorStore, withGesture } from "@/state/editor";
import { EditGesture } from "@/state/gesture";
import { Button, Field, IconButton, NumberField, Section, Select } from "@/ui";
import {
  aspectValue,
  type CropField,
  displaySize,
  FULL_CROP,
  fitRectToAspect,
  flipRect,
  invertAspect,
  reduceRatio,
  rotateRectCw,
  setPixelField,
  toPixels,
} from "./cropMath";

export interface CropPanelProps {
  media: ImportedMedia;
  project: Project;
}

interface AspectOption {
  id: string;
  label: string;
  /** Width and height of the preset; null for free, or "original" for the source's own shape. */
  ratio: [number, number] | "original" | null;
}

/** Aspect presets for the crop lock. Ratios are width over height. */
export const ASPECT_OPTIONS: readonly AspectOption[] = [
  { id: "free", label: "", ratio: null },
  { id: "original", label: "", ratio: "original" },
  { id: "1:1", label: "1:1", ratio: [1, 1] },
  { id: "4:5", label: "4:5", ratio: [4, 5] },
  { id: "9:16", label: "9:16", ratio: [9, 16] },
  { id: "16:9", label: "16:9", ratio: [16, 9] },
  { id: "4:3", label: "4:3", ratio: [4, 3] },
];

/** The preset id that matches the stored aspect, or "custom" when none does. */
export function aspectPresetId(
  aspect: Rational | null,
  size: { width: number; height: number },
): string {
  if (!aspect) return "free";
  const original = reduceRatio(size.width, size.height);
  if (aspect.num === original.num && aspect.den === original.den) return "original";
  const match = ASPECT_OPTIONS.find(
    (option) =>
      Array.isArray(option.ratio) &&
      option.ratio[0] === aspect.num &&
      option.ratio[1] === aspect.den,
  );
  return match?.id ?? "custom";
}

/** The stored aspect for a preset id, in lowest terms; null for free. */
export function aspectForPreset(
  id: string,
  size: { width: number; height: number },
): Rational | null {
  const option = ASPECT_OPTIONS.find((item) => item.id === id);
  if (!option || option.ratio === null) return null;
  if (option.ratio === "original") return reduceRatio(size.width, size.height);
  return reduceRatio(option.ratio[0], option.ratio[1]);
}

/**
 * The clockwise turn the picture shows on screen. Rotation is stored before the flips, so with exactly one flip the
 * stored turn reads the other way: a stored 270 after one flip looks like 90.
 */
export function screenRotation(transform: {
  rotation: number;
  flipH: boolean;
  flipV: boolean;
}): number {
  return transform.flipH !== transform.flipV
    ? (360 - transform.rotation) % 360
    : transform.rotation;
}

/**
 * Crop tool controls: aspect lock, rotation and flips (each remaps the crop so it keeps the same picture content),
 * the crop in display pixels and a reset. Every change is one undo step; a numeric field drag is grouped.
 */
export function CropPanel({ media, project }: CropPanelProps) {
  const { t } = useTranslation();
  const [gesture] = useState(() => new EditGesture());
  useEffect(() => () => gesture.end(), [gesture]);

  const transform = project.transform;
  const size = displaySize(media.info.video, transform.rotation);
  const pixels = toPixels(transform.crop, size);
  const presetId = aspectPresetId(transform.cropAspect, size);
  const state = () => useEditorStore.getState();

  const applyAspect = (id: string) => {
    const next = aspectForPreset(id, size);
    const value = aspectValue(next);
    withGesture(() => {
      const editor = state();
      editor.setCropAspect(next);
      if (value !== null) {
        editor.setCrop(fitRectToAspect(editor.project?.transform.crop ?? FULL_CROP, value, size));
      }
    });
  };

  const rotateBy = (degrees: 90 | -90) => {
    withGesture(() => {
      const editor = state();
      const current = editor.project?.transform;
      if (!current) return;
      // Rotation is stored before the flips are applied (stored space, then rotation, then flips), so with one flip a
      // stored turn reads the other way on screen. The stored turn is reversed then, and the picture and the crop both
      // turn clockwise on screen for a Rotate right.
      editor.rotate(current.flipH !== current.flipV ? (-degrees as 90 | -90) : degrees);
      editor.setCrop(rotateRectCw(current.crop, degrees));
      if (current.cropAspect) editor.setCropAspect(invertAspect(current.cropAspect));
    });
  };

  const flip = (axis: "horizontal" | "vertical") => {
    withGesture(() => {
      const editor = state();
      const current = editor.project?.transform.crop;
      if (!current) return;
      if (axis === "horizontal") editor.toggleFlipH();
      else editor.toggleFlipV();
      editor.setCrop(flipRect(current, axis));
    });
  };

  const reset = () => {
    withGesture(() => {
      const editor = state();
      const value = aspectValue(editor.project?.transform.cropAspect ?? null);
      editor.setCrop(value !== null ? fitRectToAspect(FULL_CROP, value, size) : FULL_CROP);
    });
  };

  /** One numeric field: `final` closes the gesture (Enter, blur, the end of a scrub). */
  const editField = (field: CropField, value: number, final: boolean) => {
    const editor = state();
    const current = editor.project?.transform.crop;
    if (!current) return;
    const cropAspect = aspectValue(editor.project?.transform.cropAspect ?? null);
    gesture.update(() => editor.setCrop(setPixelField(current, field, value, size, cropAspect)));
    if (final) gesture.end();
  };

  const aspectOptions = ASPECT_OPTIONS.map((option) => ({
    value: option.id,
    label:
      option.id === "free"
        ? t("crop.aspectFree")
        : option.id === "original"
          ? t("crop.aspectOriginal")
          : option.label,
  }));
  if (presetId === "custom") {
    aspectOptions.push({ value: "custom", label: t("crop.aspectCustom") });
  }

  const fieldLimit = (value: number, total: number) => Math.max(0, total - value);

  return (
    <>
      <Section title={t("crop.frame")}>
        <Field label={t("crop.aspect")}>
          <Select
            aria-label={t("crop.aspect")}
            value={presetId}
            onValueChange={applyAspect}
            options={aspectOptions.map((option) => ({
              ...option,
              disabled: option.value === "custom",
            }))}
          />
        </Field>
        <Field label={t("crop.rotate")}>
          <div className="flex items-center gap-1">
            <IconButton
              icon={RotateCcw}
              size="sm"
              aria-label={t("crop.rotateLeft")}
              onClick={() => rotateBy(-90)}
            />
            <IconButton
              icon={RotateCw}
              size="sm"
              aria-label={t("crop.rotateRight")}
              onClick={() => rotateBy(90)}
            />
            <span className="tabular ml-1 text-sm text-fg-muted">
              {t("crop.rotationValue", { value: screenRotation(transform) })}
            </span>
          </div>
        </Field>
        <Field label={t("crop.flip")}>
          <div className="flex items-center gap-1">
            <IconButton
              icon={FlipHorizontal2}
              size="sm"
              aria-label={t("crop.flipHorizontal")}
              pressed={transform.flipH}
              onClick={() => flip("horizontal")}
            />
            <IconButton
              icon={FlipVertical2}
              size="sm"
              aria-label={t("crop.flipVertical")}
              pressed={transform.flipV}
              onClick={() => flip("vertical")}
            />
          </div>
        </Field>
      </Section>

      <Section title={t("crop.position")}>
        <CropNumber
          label={t("crop.x")}
          value={pixels.x}
          min={0}
          max={fieldLimit(pixels.width, size.width)}
          onEdit={(value, final) => editField("x", value, final)}
        />
        <CropNumber
          label={t("crop.y")}
          value={pixels.y}
          min={0}
          max={fieldLimit(pixels.height, size.height)}
          onEdit={(value, final) => editField("y", value, final)}
        />
        <CropNumber
          label={t("crop.width")}
          value={pixels.width}
          min={1}
          max={fieldLimit(pixels.x, size.width) || 1}
          onEdit={(value, final) => editField("width", value, final)}
        />
        <CropNumber
          label={t("crop.height")}
          value={pixels.height}
          min={1}
          max={fieldLimit(pixels.y, size.height) || 1}
          onEdit={(value, final) => editField("height", value, final)}
        />
        <div className="pt-1">
          <Button variant="secondary" size="sm" onClick={reset}>
            {t("crop.reset")}
          </Button>
        </div>
      </Section>
    </>
  );
}

function CropNumber({
  label,
  value,
  min,
  max,
  onEdit,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  onEdit: (value: number, final: boolean) => void;
}) {
  return (
    <NumberField
      label={label}
      value={value}
      min={min}
      max={max}
      step={1}
      decimals={0}
      unit="px"
      onChange={(next) => onEdit(next, false)}
      onCommit={(next) => onEdit(next, true)}
    />
  );
}
