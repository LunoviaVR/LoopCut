import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import { fileNameOf } from "@/features/import/formats";
import type { LutInfo } from "@/ipc/bindings/LutInfo";
import { lutInspect, pickLutFile } from "@/ipc/commands";
import { useEditorStore } from "@/state/editor";
import { Button, Section } from "@/ui";
import { GradeSlider } from "./gradeControls";

/**
 * The 3D or 1D LUT the grade applies. Loading parses the file first (`lut_inspect`), so a broken file is refused
 * before the project refers to it. Removing clears the path; the strength slider scales the LUT.
 */
export function LutSection() {
  const { t } = useTranslation();
  const lutPath = useEditorStore((state) => state.project?.grade.lutPath ?? null);
  const [info, setInfo] = useState<LutInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Describe the LUT the project refers to, including one restored with the project.
  useEffect(() => {
    setInfo(null);
    setError(null);
    if (!lutPath) return;
    let cancelled = false;
    lutInspect(lutPath).then(
      (next) => {
        if (!cancelled) setInfo(next);
      },
      (failure: unknown) => {
        if (!cancelled) setError(messageOf(failure));
      },
    );
    return () => {
      cancelled = true;
    };
  }, [lutPath]);

  const load = async () => {
    setError(null);
    let path: string | null;
    try {
      path = await pickLutFile();
    } catch (failure) {
      setError(messageOf(failure));
      return;
    }
    if (!path) return;
    setBusy(true);
    try {
      await lutInspect(path);
      useEditorStore.getState().setGrade({ lutPath: path });
    } catch (failure) {
      setError(messageOf(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title={t("color.lut.title")}>
      {lutPath ? (
        <div className="flex min-w-0 flex-col gap-1">
          <p className="truncate text-sm text-fg" title={lutPath}>
            {info?.title ?? fileNameOf(lutPath)}
          </p>
          {info ? (
            <p className="tabular text-xs text-fg-muted">
              {t("color.lut.info", {
                size: info.size,
                kind: info.kind === "3d" ? t("color.lut.kind3d") : t("color.lut.kind1d"),
              })}
            </p>
          ) : null}
          {error ? (
            <p role="alert" className="text-xs text-danger">
              {t("color.lut.failed", { message: error })}
            </p>
          ) : null}
          <GradeSlider
            field="lutStrength"
            label={t("color.lut.strength")}
            min={0}
            max={1}
            step={0.01}
            decimals={2}
            defaultValue={1}
          />
          <div className="flex justify-end">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => useEditorStore.getState().setGrade({ lutPath: null })}
            >
              {t("color.lut.remove")}
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-1">
          <p className="text-xs text-fg-muted">{t("color.lut.none")}</p>
          <div>
            <Button variant="secondary" size="sm" loading={busy} onClick={() => void load()}>
              {t("color.lut.load")}
            </Button>
          </div>
          {error ? (
            <p role="alert" className="text-xs text-danger">
              {t("color.lut.failed", { message: error })}
            </p>
          ) : null}
        </div>
      )}
    </Section>
  );
}
