import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { Grade } from "@/ipc/bindings/Grade";
import type { HslAdjust } from "@/ipc/bindings/HslAdjust";
import { cn } from "@/lib/cn";
import { useEditorStore } from "@/state/editor";
import { useEditGesture } from "@/state/gesture";
import { Section, SliderField } from "@/ui";

type HslField = keyof HslAdjust;

/** The eight hue bands, in the order the grade stores them, with a swatch hue for each. */
const BANDS: readonly { labelKey: string; hue: number }[] = [
  { labelKey: "color.hsl.red", hue: 0 },
  { labelKey: "color.hsl.orange", hue: 30 },
  { labelKey: "color.hsl.yellow", hue: 60 },
  { labelKey: "color.hsl.green", hue: 120 },
  { labelKey: "color.hsl.aqua", hue: 180 },
  { labelKey: "color.hsl.blue", hue: 240 },
  { labelKey: "color.hsl.purple", hue: 270 },
  { labelKey: "color.hsl.magenta", hue: 300 },
];

/** Writes one field of one band, keeping the other bands as they are. */
function writeBand(band: number, field: HslField, value: number): void {
  const hsl = useEditorStore.getState().project?.grade.hsl;
  if (!hsl) return;
  const next = hsl.map((adjust, index) =>
    index === band ? { ...adjust, [field]: value } : adjust,
  ) as Grade["hsl"];
  useEditorStore.getState().setGrade({ hsl: next });
}

/** Hue, saturation and luminance per band: pick a swatch, then move its sliders. */
export function HslSection() {
  const { t } = useTranslation();
  const [band, setBand] = useState(0);
  const hsl = useEditorStore((state) => state.project?.grade.hsl);
  const adjust = hsl?.[band] ?? { hue: 0, saturation: 0, luminance: 0 };
  const bandName = t(BANDS[band]?.labelKey ?? "color.hsl.red");

  return (
    <Section title={t("color.hsl.title")}>
      <fieldset
        aria-label={t("color.hsl.bands")}
        className="m-0 grid min-w-0 grid-cols-8 gap-1 border-0 p-0"
      >
        {BANDS.map((item, index) => (
          <button
            key={item.labelKey}
            type="button"
            aria-label={t(item.labelKey)}
            aria-pressed={band === index}
            onClick={() => setBand(index)}
            className={cn(
              "h-6 w-full rounded-control border border-transparent outline-none",
              "focus-visible:outline-2 focus-visible:outline-accent",
              "aria-pressed:border-fg aria-pressed:shadow-[0_0_0_1px_var(--color-panel)]",
            )}
            style={{ backgroundColor: `hsl(${item.hue} 70% 52%)` }}
          />
        ))}
      </fieldset>
      <p className="text-xs text-fg-muted">{t("color.hsl.selected", { name: bandName })}</p>
      <BandSlider band={band} field="hue" adjust={adjust} />
      <BandSlider band={band} field="saturation" adjust={adjust} />
      <BandSlider band={band} field="luminance" adjust={adjust} />
    </Section>
  );
}

function BandSlider({ band, field, adjust }: { band: number; field: HslField; adjust: HslAdjust }) {
  const { t } = useTranslation();
  const edit = useEditGesture();
  const isHue = field === "hue";
  const labelKey = `color.hsl.${field}`;
  return (
    <SliderField
      label={t(labelKey)}
      value={adjust[field]}
      min={isHue ? -180 : -1}
      max={isHue ? 180 : 1}
      step={isHue ? 1 : 0.01}
      decimals={isHue ? 0 : 2}
      unit={isHue ? "°" : undefined}
      defaultValue={0}
      onChange={(value) => edit.change(() => writeBand(band, field, value))}
      onCommit={(value) => edit.commit(() => writeBand(band, field, value))}
    />
  );
}
