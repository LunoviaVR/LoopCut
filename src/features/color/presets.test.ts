import { describe, expect, it } from "vitest";
import { defaultGrade } from "@/domain/defaults";
import type { Grade } from "@/ipc/bindings/Grade";
import { applyGradePreset, GRADE_PRESETS, presetMatches, resetGrade } from "./presets";

/** A grade with a little of everything set, to see what a preset and a reset keep and clear. */
function busyGrade(): Grade {
  return {
    ...defaultGrade(),
    enabled: false,
    exposure: 1.5,
    contrast: -0.4,
    lift: [0.2, 0.1, 0],
    lutPath: "C:\\LUTs\\warm.cube",
    lutStrength: 0.6,
    sharpen: 0.5,
    curves: {
      master: [{ x: 0.5, y: 0.7 }],
      red: [],
      green: [],
      blue: [],
    },
    preset: "cool",
  };
}

describe("grade presets", () => {
  it("has unique ids and names for each preset", () => {
    const ids = GRADE_PRESETS.map((preset) => preset.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(GRADE_PRESETS.every((preset) => preset.labelKey.startsWith("color.presets."))).toBe(
      true,
    );
  });

  it("keeps every preset value inside the ranges the grade accepts", () => {
    for (const preset of GRADE_PRESETS) {
      const values = preset.values;
      for (const key of ["exposure"] as const) {
        const value = values[key];
        if (value !== undefined) expect(Math.abs(value)).toBeLessThanOrEqual(4);
      }
      for (const key of [
        "contrast",
        "highlights",
        "shadows",
        "saturation",
        "vibrance",
        "temperature",
        "tint",
        "sharpen",
        "vignette",
      ] as const) {
        const value = values[key];
        if (value !== undefined)
          expect(value).toBeGreaterThanOrEqual(key === "sharpen" || key === "vignette" ? 0 : -1);
        if (value !== undefined) expect(value).toBeLessThanOrEqual(1);
      }
      for (const channel of values.lift ?? []) expect(Math.abs(channel)).toBeLessThanOrEqual(1);
      for (const channel of values.gain ?? []) expect(channel).toBeGreaterThanOrEqual(0);
    }
  });

  it("applies a preset: its named values and the preset id, and neutral for the other basic and effect fields", () => {
    const next = applyGradePreset(busyGrade(), "punchy");
    expect(next).not.toBeNull();
    expect(next?.contrast).toBe(0.25);
    expect(next?.saturation).toBe(0.2);
    expect(next?.preset).toBe("punchy");
    // Fields the preset does not name go back to neutral, so the grade matches the pressed preset.
    expect(next?.exposure).toBe(0);
    expect(next?.tint).toBe(0);
    expect(next?.sharpen).toBe(0.2);
    expect(next?.vignette).toBe(0);
  });

  it("clears the values a previous preset set when another preset is applied", () => {
    const punchy = applyGradePreset(busyGrade(), "punchy");
    const faded = punchy ? applyGradePreset(punchy, "faded") : null;
    expect(faded?.preset).toBe("faded");
    expect(faded?.contrast).toBe(-0.25);
    expect(faded?.vibrance).toBe(0);
    expect(faded?.sharpen).toBe(0);
    expect(faded?.shadows).toBe(0);
  });

  it("keeps the wheels, curves and hue bands the user set", () => {
    const hsl = defaultGrade().hsl;
    hsl[5] = { hue: 20, saturation: 0.1, luminance: -0.2 };
    const gamma: Grade["gamma"] = [1.2, 1, 0.9];
    const before = { ...busyGrade(), hsl, gamma };
    const next = applyGradePreset(before, "warmFilm");
    expect(next?.lift).toEqual(before.lift);
    expect(next?.gamma).toEqual(before.gamma);
    expect(next?.gain).toEqual(before.gain);
    expect(next?.curves).toEqual(before.curves);
    expect(next?.hsl).toEqual(before.hsl);
  });

  it("never names a wheel, a curve or a hue band, so a preset cannot undo that work", () => {
    for (const preset of GRADE_PRESETS) {
      expect(preset.values.lift).toBeUndefined();
      expect(preset.values.gamma).toBeUndefined();
      expect(preset.values.gain).toBeUndefined();
      expect(preset.values.curves).toBeUndefined();
      expect(preset.values.hsl).toBeUndefined();
    }
  });

  it("keeps the bypass flag and the LUT when a preset is applied", () => {
    const next = applyGradePreset(busyGrade(), "warmFilm");
    expect(next?.enabled).toBe(false);
    expect(next?.lutPath).toBe("C:\\LUTs\\warm.cube");
    expect(next?.lutStrength).toBe(0.6);
  });

  it("returns null for an unknown preset", () => {
    expect(applyGradePreset(busyGrade(), "missing")).toBeNull();
  });

  it("applies the black and white preset as full desaturation", () => {
    expect(applyGradePreset(busyGrade(), "blackAndWhite")?.saturation).toBe(-1);
  });
});

describe("preset match", () => {
  it("reads a preset as applied until one of its basic or effect fields is changed by hand", () => {
    const punchy = applyGradePreset(busyGrade(), "punchy");
    if (!punchy) throw new Error("punchy is a preset");
    expect(presetMatches(punchy, "punchy")).toBe(true);
    expect(presetMatches({ ...punchy, exposure: 0.5 }, "punchy")).toBe(false);
    expect(presetMatches(punchy, "faded")).toBe(false);
  });

  it("ignores the wheels and curves when it reads a preset as applied", () => {
    const punchy = applyGradePreset(busyGrade(), "punchy");
    if (!punchy) throw new Error("punchy is a preset");
    expect(presetMatches({ ...punchy, lift: [0.3, 0, 0] }, "punchy")).toBe(true);
  });
});

describe("reset all", () => {
  it("returns every color control to neutral, the LUT included, and keeps only the bypass flag", () => {
    const reset = resetGrade(busyGrade());
    expect(reset).toEqual({ ...defaultGrade(), enabled: false });
    expect(reset.lutPath).toBeNull();
    expect(reset.preset).toBeNull();
  });
});
