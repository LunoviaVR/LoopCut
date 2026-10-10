import { useTranslation } from "react-i18next";
import type { Grade } from "@/ipc/bindings/Grade";
import { useEditorStore } from "@/state/editor";
import { Button, Section, Switch, selectedChipClass } from "@/ui";
import { ColorCompareToggle } from "./ColorCompareToggle";
import { CurvesSection } from "./CurvesSection";
import { GradeSlider, type NumericGradeKey } from "./gradeControls";
import { HslSection } from "./HslSection";
import { LutSection } from "./LutSection";
import { applyGradePreset, GRADE_PRESETS, presetMatches, resetGrade } from "./presets";
import { WheelsSection } from "./WheelsSection";

interface BasicField {
  field: NumericGradeKey;
  labelKey: string;
  min: number;
  max: number;
}

/** Exposure is in stops; the others are -1 to 1 (0 neutral). */
const BASIC_FIELDS: readonly BasicField[] = [
  { field: "exposure", labelKey: "color.fields.exposure", min: -4, max: 4 },
  { field: "contrast", labelKey: "color.fields.contrast", min: -1, max: 1 },
  { field: "highlights", labelKey: "color.fields.highlights", min: -1, max: 1 },
  { field: "shadows", labelKey: "color.fields.shadows", min: -1, max: 1 },
  { field: "saturation", labelKey: "color.fields.saturation", min: -1, max: 1 },
  { field: "vibrance", labelKey: "color.fields.vibrance", min: -1, max: 1 },
  { field: "temperature", labelKey: "color.fields.temperature", min: -1, max: 1 },
  { field: "tint", labelKey: "color.fields.tint", min: -1, max: 1 },
];

const EFFECT_FIELDS: readonly BasicField[] = [
  { field: "sharpen", labelKey: "color.fields.sharpen", min: 0, max: 1 },
  { field: "vignette", labelKey: "color.fields.vignette", min: 0, max: 1 },
];

/**
 * Color tool: the grade's bypass, before and after, presets, then the basic controls, the three wheels, the tone
 * curves, the hue bands, the LUT and the effects. Every control writes the project grade; a drag or typed value is one
 * undo step, and a preset or a reset is one step too.
 */
export function ColorPanel() {
  const { t } = useTranslation();
  const grade = useEditorStore((state) => state.project?.grade ?? null);
  if (!grade) return null;

  const setGrade = (patch: Partial<Grade>) => useEditorStore.getState().setGrade(patch);
  const current = () => useEditorStore.getState().project?.grade ?? grade;

  return (
    <>
      <Section title={t("color.grade.title")}>
        <Switch
          checked={!grade.enabled}
          onCheckedChange={(bypass) => setGrade({ enabled: !bypass })}
          label={t("color.grade.bypass")}
          description={t("color.grade.bypassHint")}
        />
        <ColorCompareToggle />
      </Section>

      <Section title={t("color.presets.title")}>
        <fieldset
          aria-label={t("color.presets.title")}
          className="m-0 flex min-w-0 flex-wrap gap-1 border-0 p-0"
        >
          {GRADE_PRESETS.map((preset) => (
            <Button
              key={preset.id}
              variant="secondary"
              size="sm"
              aria-pressed={presetMatches(grade, preset.id)}
              className={selectedChipClass}
              onClick={() => {
                const next = applyGradePreset(current(), preset.id);
                if (next) setGrade(next);
              }}
            >
              {t(preset.labelKey)}
            </Button>
          ))}
        </fieldset>
        <p className="text-xs text-fg-muted">{t("color.presets.hint")}</p>
        <div className="flex justify-end">
          <Button variant="ghost" size="sm" onClick={() => setGrade(resetGrade(current()))}>
            {t("color.presets.resetAll")}
          </Button>
        </div>
      </Section>

      <Section title={t("color.basic.title")}>
        {BASIC_FIELDS.map((item) => (
          <GradeSlider
            key={item.field}
            field={item.field}
            label={t(item.labelKey)}
            min={item.min}
            max={item.max}
            step={0.01}
            decimals={2}
            defaultValue={0}
          />
        ))}
      </Section>

      <WheelsSection />
      <CurvesSection />
      <HslSection />
      <LutSection />

      <Section title={t("color.effects.title")}>
        {EFFECT_FIELDS.map((item) => (
          <GradeSlider
            key={item.field}
            field={item.field}
            label={t(item.labelKey)}
            min={item.min}
            max={item.max}
            step={0.01}
            decimals={2}
            defaultValue={0}
          />
        ))}
      </Section>
    </>
  );
}
