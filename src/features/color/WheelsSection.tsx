import { useTranslation } from "react-i18next";
import type { Grade } from "@/ipc/bindings/Grade";
import { useEditorStore } from "@/state/editor";
import { Section } from "@/ui";
import { ColorWheel } from "./ColorWheel";
import type { Rgb, WheelSpace } from "./wheelMath";
import { NEUTRAL_VALUES } from "./wheelMath";

type WheelKey = "lift" | "gamma" | "gain";

interface WheelDefinition {
  key: WheelKey;
  labelKey: string;
  space: WheelSpace;
  masterMin: number;
  masterMax: number;
  masterStep: number;
}

/** Lift is additive; gamma and gain are multiplicative and read through logarithms (see wheelMath). */
const WHEELS: readonly WheelDefinition[] = [
  {
    key: "lift",
    labelKey: "color.wheels.lift",
    space: "linear",
    masterMin: -1,
    masterMax: 1,
    masterStep: 0.01,
  },
  {
    key: "gamma",
    labelKey: "color.wheels.gamma",
    space: "log",
    masterMin: 0.1,
    masterMax: 4,
    masterStep: 0.01,
  },
  {
    key: "gain",
    labelKey: "color.wheels.gain",
    space: "log",
    masterMin: 0,
    masterMax: 4,
    masterStep: 0.01,
  },
];

/** The three color wheels of the grade. Each wheel writes its own channel triple. */
export function WheelsSection() {
  const { t } = useTranslation();
  return (
    <Section title={t("color.wheels.title")}>
      <div className="grid grid-cols-3 gap-1">
        {WHEELS.map((wheel) => (
          <WheelFromGrade key={wheel.key} definition={wheel} />
        ))}
      </div>
    </Section>
  );
}

function WheelFromGrade({ definition }: { definition: WheelDefinition }) {
  const { t } = useTranslation();
  const stored = useEditorStore((state) => state.project?.grade[definition.key]);
  const values = (stored ?? NEUTRAL_VALUES(definition.space)) as Rgb;
  const write = (next: Rgb) => {
    useEditorStore.getState().setGrade({ [definition.key]: next } as Partial<Grade>);
  };
  return (
    <ColorWheel
      label={t(definition.labelKey)}
      space={definition.space}
      values={values}
      masterMin={definition.masterMin}
      masterMax={definition.masterMax}
      masterStep={definition.masterStep}
      onChange={write}
      onCommit={write}
    />
  );
}
