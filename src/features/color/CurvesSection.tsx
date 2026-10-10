import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { CurvePoint } from "@/ipc/bindings/CurvePoint";
import type { Curves } from "@/ipc/bindings/Curves";
import { useEditorStore } from "@/state/editor";
import { Button, Section, SegmentedControl } from "@/ui";
import { CurveEditor } from "./CurveEditor";

type Channel = keyof Curves;

const CHANNELS: readonly { value: Channel; labelKey: string }[] = [
  { value: "master", labelKey: "color.curves.master" },
  { value: "red", labelKey: "color.curves.red" },
  { value: "green", labelKey: "color.curves.green" },
  { value: "blue", labelKey: "color.curves.blue" },
];

/** Writes one channel's points into the grade, keeping the other channels as they are. */
function writeCurve(channel: Channel, points: CurvePoint[]): void {
  const curves = useEditorStore.getState().project?.grade.curves;
  if (!curves) return;
  useEditorStore.getState().setGrade({ curves: { ...curves, [channel]: points } });
}

/** The tone curves: the master curve and one per color channel, with a reset for the shown channel. */
export function CurvesSection() {
  const { t } = useTranslation();
  const [channel, setChannel] = useState<Channel>("master");
  const curves = useEditorStore((state) => state.project?.grade.curves);
  const points = curves?.[channel] ?? [];
  const name = t(
    CHANNELS.find((item) => item.value === channel)?.labelKey ?? "color.curves.master",
  );

  return (
    <Section title={t("color.curves.title")}>
      <SegmentedControl<Channel>
        className="w-full"
        aria-label={t("color.curves.channel")}
        value={channel}
        onValueChange={setChannel}
        items={CHANNELS.map((item) => ({ value: item.value, label: t(item.labelKey) }))}
      />
      <CurveEditor
        key={channel}
        label={name}
        points={points}
        onCommit={(next) => writeCurve(channel, next)}
      />
      <div className="flex justify-end">
        <Button
          variant="ghost"
          size="sm"
          disabled={points.length === 0}
          onClick={() => writeCurve(channel, [])}
        >
          {t("color.curves.reset")}
        </Button>
      </div>
    </Section>
  );
}
