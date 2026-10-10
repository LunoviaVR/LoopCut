import { Popover } from "radix-ui";
import { useEffect, useId, useRef, useState } from "react";
import { RgbaColorPicker } from "react-colorful";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import { fontsList } from "@/ipc/commands";
import { cn } from "@/lib/cn";
import { useEditGesture } from "@/state/gesture";
import { Field, Select, SliderField } from "@/ui";
import { rgbaCss } from "./captionGeometry";

type Rgba = [number, number, number, number];

export interface CaptionSliderProps {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  decimals: number;
  defaultValue: number;
  unit?: string;
  /** Applies one value. Drags and typed values are grouped into one undo step. */
  onWrite: (value: number) => void;
}

/** A SliderField whose live values and final value are grouped into one undo step per gesture. */
export function CaptionSlider({ onWrite, ...rest }: CaptionSliderProps) {
  const edit = useEditGesture();
  return (
    <SliderField
      {...rest}
      onChange={(next) => edit.change(() => onWrite(next))}
      onCommit={(next) => edit.commit(() => onWrite(next))}
    />
  );
}

export interface ColorFieldProps {
  label: string;
  value: Rgba;
  onChange: (value: Rgba) => void;
}

/** A color swatch that opens an RGBA picker. The picker's drags are one undo step, closed when the popover closes. */
export function ColorField({ label, value, onChange }: ColorFieldProps) {
  const [open, setOpen] = useState(false);
  const edit = useEditGesture();
  const swatchId = useId();
  const [r, g, b, a] = value;
  return (
    <Field label={label} htmlFor={swatchId}>
      <Popover.Root
        open={open}
        onOpenChange={(next) => {
          if (!next) edit.commit(() => {});
          setOpen(next);
        }}
      >
        <Popover.Trigger asChild>
          <button
            id={swatchId}
            type="button"
            className={cn(
              "block h-6 w-12 rounded-control border border-line-control outline-none hover:border-fg-muted",
              "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent",
            )}
            style={{ background: rgbaCss(value) }}
          />
        </Popover.Trigger>
        <Popover.Portal>
          <Popover.Content
            align="end"
            sideOffset={6}
            collisionPadding={8}
            className="z-50 rounded-control bg-overlay p-2 shadow-popover"
          >
            <RgbaColorPicker
              color={{ r, g, b, a: a / 255 }}
              onChange={(next) =>
                edit.change(() =>
                  onChange([
                    Math.round(next.r),
                    Math.round(next.g),
                    Math.round(next.b),
                    Math.round(next.a * 255),
                  ]),
                )
              }
            />
          </Popover.Content>
        </Popover.Portal>
      </Popover.Root>
    </Field>
  );
}

/**
 * Font families the rasterizer knows, fetched once for the session. A failed request is forgotten, so the next field
 * asks again instead of showing the failure for the rest of the session.
 */
let fontsRequest: Promise<string[]> | null = null;
function loadFonts(): Promise<string[]> {
  fontsRequest ??= fontsList().catch((error: unknown) => {
    fontsRequest = null;
    throw error;
  });
  return fontsRequest;
}

/** Test hook: forgets the cached font list so a test can ask again. */
export function resetFontCache(): void {
  fontsRequest = null;
}

export interface FontFieldProps {
  value: string;
  onChange: (family: string) => void;
}

/**
 * Font family: a search box over the installed families and a select. The current family stays in the list even when
 * the list has not loaded or does not contain it.
 */
export function FontField({ value, onChange }: FontFieldProps) {
  const { t } = useTranslation();
  const [fonts, setFonts] = useState<string[]>([value]);
  const [query, setQuery] = useState("");
  const [failed, setFailed] = useState<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    loadFonts().then(
      (list) => {
        if (mounted.current) setFonts(list);
      },
      (failure: unknown) => {
        if (mounted.current) setFailed(messageOf(failure));
      },
    );
    return () => {
      mounted.current = false;
    };
  }, []);

  const needle = query.trim().toLowerCase();
  const families = fonts.filter((name) => name.toLowerCase().includes(needle));
  if (!families.includes(value) && (needle === "" || value.toLowerCase().includes(needle))) {
    families.unshift(value);
  }

  return (
    <div className="flex min-w-0 flex-col gap-1">
      <input
        type="search"
        aria-label={t("captions.font.search")}
        placeholder={t("captions.font.searchPlaceholder")}
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        className="h-7 w-full min-w-0 rounded-control border border-line-control bg-raised px-2 text-sm text-fg outline-none placeholder:text-fg-muted hover:border-fg-muted focus:border-accent"
      />
      <Select
        aria-label={t("captions.font.family")}
        value={value}
        onValueChange={onChange}
        options={families.map((name) => ({ value: name, label: name }))}
      />
      {failed ? (
        <p role="alert" className="text-xs text-warn">
          {t("captions.font.failed", { message: failed })}
        </p>
      ) : null}
    </div>
  );
}
