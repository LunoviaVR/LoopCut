import { Bold, CaseUpper, Italic, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { Caption } from "@/ipc/bindings/Caption";
import type { CaptionAnimation } from "@/ipc/bindings/CaptionAnimation";
import type { CaptionStyle } from "@/ipc/bindings/CaptionStyle";
import type { SlideEdge } from "@/ipc/bindings/SlideEdge";
import type { TextAlign } from "@/ipc/bindings/TextAlign";
import { useEditorStore } from "@/state/editor";
import { useEditGesture } from "@/state/gesture";
import {
  Button,
  Field,
  IconButton,
  NumberField,
  Section,
  SegmentedControl,
  Select,
  Switch,
} from "@/ui";
import { CaptionSlider, ColorField, FontField } from "./CaptionControls";
import { PRESET_Y } from "./captionFactory";
import type { OutputSize } from "./captionGeometry";
import { clampCaptionCentre } from "./captionGeometry";
import { setCaptionEnd, setCaptionStart } from "./captionTimeline";

const ANIMATION_KINDS = ["none", "fade", "pop", "slide", "typewriter"] as const;
type AnimationKind = (typeof ANIMATION_KINDS)[number];

const SLIDE_EDGES: readonly SlideEdge[] = ["left", "right", "top", "bottom"];

/** Parameters a new animation starts with, per kind. */
function animationOf(kind: AnimationKind): CaptionAnimation {
  switch (kind) {
    case "none":
      return { kind: "none" };
    case "fade":
      return { kind: "fade", fadeIn: 0.25, fadeOut: 0.25 };
    case "pop":
      return { kind: "pop", duration: 0.2 };
    case "slide":
      return { kind: "slide", from: "bottom", duration: 0.3 };
    case "typewriter":
      return { kind: "typewriter", charsPerSecond: 12 };
  }
}

export interface CaptionEditorProps {
  caption: Caption;
  /** Length of the clip in source seconds: the caption's range stays inside it. */
  duration: number;
  output: OutputSize;
  /** Frame count of the plan, for the playhead; null before a plan exists. */
  playheadSource: number | null;
  onDelete: (id: string) => void;
}

/**
 * Editor for the selected caption. Every change writes that caption in the store: a drag, a typed value or a slider
 * is one undo step, and a switch, a choice or a button is one step of its own.
 */
export function CaptionEditor({
  caption,
  duration,
  output,
  playheadSource,
  onDelete,
}: CaptionEditorProps) {
  const { t } = useTranslation();
  const id = caption.id;
  const { style, animation } = caption;
  const shadow = style.shadow;
  const edit = useEditGesture();
  const textEdit = useEditGesture();

  const latest = (): Caption | undefined =>
    useEditorStore.getState().project?.captions.find((item) => item.id === id);
  const write = (patch: Partial<Omit<Caption, "id">>) =>
    useEditorStore.getState().updateCaption(id, patch);
  const writeStyle = (patch: Partial<CaptionStyle>) => {
    const current = latest();
    if (current) write({ style: { ...current.style, ...patch } });
  };
  const writeAnimation = (next: CaptionAnimation) => write({ animation: next });
  const fontPx = Math.round(style.fontSize * output.height);
  const defaultFontPx = Math.round(0.12 * output.height);
  const yChoice =
    (["top", "text", "bottom"] as const).find(
      (key) => Math.abs((PRESET_Y[key] ?? 0) - caption.y) < 0.01,
    ) ?? "custom";

  const setStart = (seconds: number) => {
    const current = latest();
    if (current) write({ range: setCaptionStart(current.range, seconds, duration) });
  };
  const setEnd = (seconds: number) => {
    const current = latest();
    if (current) write({ range: setCaptionEnd(current.range, seconds, duration) });
  };
  const setCentre = (x: number, y: number) => {
    const next = clampCaptionCentre(x, y);
    write({ x: next.x, y: next.y });
  };

  const animationKind = animation.kind as AnimationKind;

  return (
    <>
      <Section title={t("captions.editor.text")}>
        <textarea
          aria-label={t("captions.editor.textLabel")}
          rows={3}
          value={caption.text}
          onChange={(event) => textEdit.change(() => write({ text: event.target.value }))}
          onBlur={() => textEdit.commit(() => {})}
          className="min-h-16 w-full resize-y rounded-control border border-line-control bg-raised px-2 py-1.5 text-sm text-fg outline-none placeholder:text-fg-muted hover:border-fg-muted focus:border-accent"
        />
        <p className="text-xs text-fg-muted">{t("captions.editor.lineBreakHint")}</p>
      </Section>

      <Section title={t("captions.editor.timing")}>
        <NumberField
          label={t("captions.editor.start")}
          value={caption.range.start}
          min={0}
          max={duration}
          step={0.05}
          decimals={2}
          unit="s"
          onChange={(value) => edit.change(() => setStart(value))}
          onCommit={(value) => edit.commit(() => setStart(value))}
        />
        <NumberField
          label={t("captions.editor.end")}
          value={caption.range.end}
          min={0}
          max={duration}
          step={0.05}
          decimals={2}
          unit="s"
          onChange={(value) => edit.change(() => setEnd(value))}
          onCommit={(value) => edit.commit(() => setEnd(value))}
        />
        <div className="flex flex-wrap gap-1">
          <Button
            variant="secondary"
            size="sm"
            disabled={playheadSource === null}
            onClick={() => playheadSource !== null && setStart(playheadSource)}
          >
            {t("captions.editor.startToPlayhead")}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            disabled={playheadSource === null}
            onClick={() => playheadSource !== null && setEnd(playheadSource)}
          >
            {t("captions.editor.endToPlayhead")}
          </Button>
        </div>
      </Section>

      <Section title={t("captions.editor.position")}>
        <SegmentedControl
          className="w-full"
          aria-label={t("captions.editor.placement")}
          value={yChoice === "custom" ? "custom" : yChoice}
          onValueChange={(choice) => {
            if (choice === "top" || choice === "text" || choice === "bottom") {
              write({ y: PRESET_Y[choice] });
            }
          }}
          items={[
            { value: "top", label: t("captions.editor.top") },
            { value: "text", label: t("captions.editor.middle") },
            { value: "bottom", label: t("captions.editor.bottom") },
          ]}
        />
        <NumberField
          label={t("captions.editor.x")}
          value={Math.round(caption.x * 100)}
          min={0}
          max={100}
          step={1}
          decimals={0}
          unit="%"
          onChange={(value) => edit.change(() => setCentre(value / 100, caption.y))}
          onCommit={(value) => edit.commit(() => setCentre(value / 100, caption.y))}
        />
        <NumberField
          label={t("captions.editor.y")}
          value={Math.round(caption.y * 100)}
          min={0}
          max={100}
          step={1}
          decimals={0}
          unit="%"
          onChange={(value) => edit.change(() => setCentre(caption.x, value / 100))}
          onCommit={(value) => edit.commit(() => setCentre(caption.x, value / 100))}
        />
      </Section>

      <Section title={t("captions.editor.font")}>
        <FontField
          value={style.fontFamily}
          onChange={(family) => writeStyle({ fontFamily: family })}
        />
        <CaptionSlider
          label={t("captions.editor.size")}
          value={fontPx}
          min={4}
          max={Math.max(8, Math.round(output.height * 0.5))}
          step={1}
          decimals={0}
          unit="px"
          defaultValue={defaultFontPx}
          onWrite={(px) => writeStyle({ fontSize: px / output.height })}
        />
        <ColorField
          label={t("captions.editor.fill")}
          value={style.fill}
          onChange={(fill) => writeStyle({ fill })}
        />
        <CaptionSlider
          label={t("captions.editor.outlineWidth")}
          value={style.strokeWidth}
          min={0}
          max={0.3}
          step={0.005}
          decimals={3}
          defaultValue={0}
          onWrite={(strokeWidth) => writeStyle({ strokeWidth })}
        />
        <ColorField
          label={t("captions.editor.outline")}
          value={style.strokeColor}
          onChange={(strokeColor) => writeStyle({ strokeColor })}
        />
        <div className="flex items-center gap-1 pt-1">
          <IconButton
            icon={Bold}
            size="sm"
            pressed={style.bold}
            aria-label={t("captions.editor.bold")}
            onClick={() => writeStyle({ bold: !style.bold })}
          />
          <IconButton
            icon={Italic}
            size="sm"
            pressed={style.italic}
            aria-label={t("captions.editor.italic")}
            onClick={() => writeStyle({ italic: !style.italic })}
          />
          <IconButton
            icon={CaseUpper}
            size="sm"
            pressed={style.uppercase}
            aria-label={t("captions.editor.uppercase")}
            onClick={() => writeStyle({ uppercase: !style.uppercase })}
          />
        </div>
        <SegmentedControl<TextAlign>
          className="w-full"
          aria-label={t("captions.editor.align")}
          value={style.align}
          onValueChange={(align) => writeStyle({ align })}
          items={[
            { value: "left", label: t("captions.editor.alignLeft") },
            { value: "center", label: t("captions.editor.alignCenter") },
            { value: "right", label: t("captions.editor.alignRight") },
          ]}
        />
      </Section>

      <Section title={t("captions.editor.shadowAndBox")}>
        <Switch
          checked={style.shadow !== null}
          onCheckedChange={(on) =>
            writeStyle({
              shadow: on ? { dx: 0.04, dy: 0.04, blur: 0.1, color: [0, 0, 0, 160] } : null,
            })
          }
          label={t("captions.editor.shadow")}
        />
        {shadow ? (
          <>
            <CaptionSlider
              label={t("captions.editor.shadowX")}
              value={shadow.dx}
              min={-0.5}
              max={0.5}
              step={0.01}
              decimals={2}
              defaultValue={0.04}
              onWrite={(dx) => writeStyle({ shadow: { ...shadow, dx } })}
            />
            <CaptionSlider
              label={t("captions.editor.shadowY")}
              value={shadow.dy}
              min={-0.5}
              max={0.5}
              step={0.01}
              decimals={2}
              defaultValue={0.04}
              onWrite={(dy) => writeStyle({ shadow: { ...shadow, dy } })}
            />
            <CaptionSlider
              label={t("captions.editor.shadowBlur")}
              value={shadow.blur}
              min={0}
              max={0.5}
              step={0.01}
              decimals={2}
              defaultValue={0.1}
              onWrite={(blur) => writeStyle({ shadow: { ...shadow, blur } })}
            />
            <ColorField
              label={t("captions.editor.shadowColor")}
              value={shadow.color}
              onChange={(color) => writeStyle({ shadow: { ...shadow, color } })}
            />
          </>
        ) : null}
        <Switch
          checked={style.background !== null}
          onCheckedChange={(on) => writeStyle({ background: on ? [0, 0, 0, 160] : null })}
          label={t("captions.editor.box")}
        />
        {style.background ? (
          <ColorField
            label={t("captions.editor.boxColor")}
            value={style.background}
            onChange={(background) => writeStyle({ background })}
          />
        ) : null}
      </Section>

      <Section title={t("captions.editor.animation")} defaultOpen={false}>
        <Field label={t("captions.editor.animationKind")}>
          <Select
            aria-label={t("captions.editor.animationKind")}
            value={animationKind}
            onValueChange={(kind) => writeAnimation(animationOf(kind as AnimationKind))}
            options={ANIMATION_KINDS.map((kind) => ({
              value: kind,
              label: t(`captions.animation.${kind}`),
            }))}
          />
        </Field>
        {animation.kind === "fade" ? (
          <>
            <CaptionSlider
              label={t("captions.editor.fadeIn")}
              value={animation.fadeIn}
              min={0}
              max={5}
              step={0.05}
              decimals={2}
              unit="s"
              defaultValue={0.25}
              onWrite={(fadeIn) => writeAnimation({ ...animation, fadeIn })}
            />
            <CaptionSlider
              label={t("captions.editor.fadeOut")}
              value={animation.fadeOut}
              min={0}
              max={5}
              step={0.05}
              decimals={2}
              unit="s"
              defaultValue={0.25}
              onWrite={(fadeOut) => writeAnimation({ ...animation, fadeOut })}
            />
          </>
        ) : null}
        {animation.kind === "pop" ? (
          <CaptionSlider
            label={t("captions.editor.popDuration")}
            value={animation.duration}
            min={0}
            max={2}
            step={0.05}
            decimals={2}
            unit="s"
            defaultValue={0.2}
            onWrite={(duration) => writeAnimation({ ...animation, duration })}
          />
        ) : null}
        {animation.kind === "slide" ? (
          <>
            <Field label={t("captions.editor.slideFrom")}>
              <Select
                aria-label={t("captions.editor.slideFrom")}
                value={animation.from}
                onValueChange={(from) => writeAnimation({ ...animation, from: from as SlideEdge })}
                options={SLIDE_EDGES.map((edge) => ({
                  value: edge,
                  label: t(`captions.slideFrom.${edge}`),
                }))}
              />
            </Field>
            <CaptionSlider
              label={t("captions.editor.slideDuration")}
              value={animation.duration}
              min={0}
              max={2}
              step={0.05}
              decimals={2}
              unit="s"
              defaultValue={0.3}
              onWrite={(duration) => writeAnimation({ ...animation, duration })}
            />
          </>
        ) : null}
        {animation.kind === "typewriter" ? (
          <CaptionSlider
            label={t("captions.editor.charsPerSecond")}
            value={animation.charsPerSecond}
            min={1}
            max={60}
            step={1}
            decimals={0}
            unit="/s"
            defaultValue={12}
            onWrite={(charsPerSecond) => writeAnimation({ ...animation, charsPerSecond })}
          />
        ) : null}
      </Section>

      <div className="flex justify-end px-3 py-3">
        <Button variant="secondary" size="sm" onClick={() => onDelete(id)}>
          <Trash2 size={16} strokeWidth={1.5} aria-hidden="true" />
          {t("captions.editor.delete")}
        </Button>
      </div>
    </>
  );
}
