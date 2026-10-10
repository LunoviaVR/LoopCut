/** Display rules for the Media panel: sizes after rotation, codec and color names. Pure, so tests can pin them. */
import type { ColorRange } from "@/ipc/bindings/ColorRange";
import type { TransferFunction } from "@/ipc/bindings/TransferFunction";
import type { VideoStreamInfo } from "@/ipc/bindings/VideoStreamInfo";
import type { YuvMatrix } from "@/ipc/bindings/YuvMatrix";

export interface DisplaySize {
  width: number;
  height: number;
  /** True for a quarter turn, where the displayed size differs from the stored (coded) size. */
  rotated: boolean;
}

/** Size as the viewer sees it. A 90 or 270 degree display rotation swaps the stored width and height. */
export function displaySize(
  video: Pick<VideoStreamInfo, "width" | "height" | "rotation">,
): DisplaySize {
  const quarterTurn = video.rotation === 90 || video.rotation === 270;
  if (quarterTurn) return { width: video.height, height: video.width, rotated: true };
  return { width: video.width, height: video.height, rotated: false };
}

const CODEC_NAMES: Readonly<Record<string, string>> = {
  h264: "H.264",
  hevc: "HEVC",
  av1: "AV1",
  vp8: "VP8",
  vp9: "VP9",
  prores: "ProRes",
};

export function codecName(codec: string): string {
  return CODEC_NAMES[codec] ?? codec.toUpperCase();
}

const MATRIX_NAMES: Readonly<Record<YuvMatrix, string>> = {
  bt601: "BT.601",
  bt709: "BT.709",
  bt2020: "BT.2020",
  rgb: "RGB",
};

export function matrixName(matrix: YuvMatrix): string {
  return MATRIX_NAMES[matrix];
}

/** Translation key suffix for a color range (`limited` or `full`). */
export function rangeKey(range: ColorRange): "rangeLimited" | "rangeFull" {
  return range === "full" ? "rangeFull" : "rangeLimited";
}

/** The HDR transfer name shown in the badge, or null for SDR. */
export function hdrFormat(transfer: TransferFunction): "PQ" | "HLG" | null {
  if (transfer === "pq") return "PQ";
  if (transfer === "hlg") return "HLG";
  return null;
}
