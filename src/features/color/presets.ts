/**
 * Grade presets and the reset. A preset sets the fields it names (contrast, saturation, temperature and similar) and
 * returns the other basic and effect fields to neutral, so those fields are exactly the preset and the pressed button
 * tells the truth. The curves, the wheels, the hue bands, the LUT and the bypass flag stay as the user left them, so a
 * preset never undoes that work. The reset returns every color control to neutral.
 */
import { defaultGrade } from "@/domain/defaults";
import type { Grade } from "@/ipc/bindings/Grade";

export interface GradePreset {
  id: string;
  /** i18n key of the name shown on the button. */
  labelKey: string;
  values: Partial<Grade>;
}

export const GRADE_PRESETS: readonly GradePreset[] = [
  {
    id: "punchy",
    labelKey: "color.presets.punchy",
    values: {
      contrast: 0.25,
      highlights: 0.15,
      shadows: -0.1,
      saturation: 0.2,
      vibrance: 0.3,
      sharpen: 0.2,
    },
  },
  {
    id: "warmFilm",
    labelKey: "color.presets.warmFilm",
    values: {
      temperature: 0.3,
      tint: 0.04,
      saturation: -0.1,
      vignette: 0.2,
    },
  },
  {
    id: "cool",
    labelKey: "color.presets.cool",
    values: {
      temperature: -0.3,
      tint: -0.03,
      saturation: -0.05,
    },
  },
  {
    id: "blackAndWhite",
    labelKey: "color.presets.blackAndWhite",
    values: { saturation: -1, contrast: 0.1 },
  },
  {
    id: "faded",
    labelKey: "color.presets.faded",
    values: {
      contrast: -0.25,
      highlights: -0.1,
      saturation: -0.2,
    },
  },
  {
    id: "highContrast",
    labelKey: "color.presets.highContrast",
    values: {
      contrast: 0.5,
      highlights: 0.2,
      shadows: -0.2,
      saturation: 0.1,
    },
  },
];

/**
 * The basic and effect fields a preset owns. A preset that does not name one of them sets it back to neutral, so a
 * second preset never inherits the first one's values.
 */
const PRESET_FIELDS = [
  "exposure",
  "contrast",
  "highlights",
  "shadows",
  "saturation",
  "vibrance",
  "temperature",
  "tint",
  "sharpen",
  "vignette",
] as const;

/** The grade after applying a preset, or null when the id is unknown. Wheels, curves, hue bands and the LUT are kept. */
export function applyGradePreset(current: Grade, presetId: string): Grade | null {
  const preset = GRADE_PRESETS.find((item) => item.id === presetId);
  if (!preset) return null;
  const neutral = defaultGrade();
  const next: Grade = { ...current, preset: preset.id };
  for (const field of PRESET_FIELDS) {
    next[field] = preset.values[field] ?? neutral[field];
  }
  return next;
}

/**
 * Whether the preset's button should read as applied: the grade names it and its basic and effect fields still hold
 * the preset's values, so a later manual edit to one of them un-presses the button.
 */
export function presetMatches(current: Grade, presetId: string): boolean {
  const preset = GRADE_PRESETS.find((item) => item.id === presetId);
  if (!preset || current.preset !== presetId) return false;
  const neutral = defaultGrade();
  return PRESET_FIELDS.every(
    (field) => current[field] === (preset.values[field] ?? neutral[field]),
  );
}

/** Every color control back at neutral, including the LUT. The bypass flag is kept. */
export function resetGrade(current: Grade): Grade {
  return { ...defaultGrade(), enabled: current.enabled };
}
