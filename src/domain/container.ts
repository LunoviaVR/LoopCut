/**
 * Friendly names for a file's container. FFmpeg reports the demuxer's name list instead of one name: every MP4 and
 * QuickTime file reads `mov,mp4,m4a,3gp,3g2,mj2`, so the file name decides which member of the family the file is.
 */

/** The lowercase extension of a file name, without the dot, or "" when it has none. */
function extensionOf(fileName: string): string {
  const dot = fileName.lastIndexOf(".");
  return dot < 0 ? "" : fileName.slice(dot + 1).toLowerCase();
}

/**
 * The name a viewer knows the container by, from FFmpeg's format name and the file name. A format this table does not
 * name shows the first name FFmpeg gave, in capitals.
 */
export function containerLabel(formatName: string, fileName: string): string {
  const names = formatName.split(",").map((name) => name.trim().toLowerCase());
  const extension = extensionOf(fileName);
  if (names.includes("mov") || names.includes("mp4")) {
    if (extension === "mov") return "MOV";
    if (extension === "3gp") return "3GP";
    if (extension === "m4a") return "M4A";
    return "MP4";
  }
  if (names.includes("matroska") || names.includes("webm")) {
    return extension === "webm" ? "WebM" : "MKV";
  }
  if (names.includes("avi")) return "AVI";
  if (names.includes("gif")) return "GIF";
  if (names.includes("mpegts")) return "MPEG-TS";
  if (names.includes("flv")) return "FLV";
  if (names.includes("asf")) return extension === "wmv" ? "WMV" : "ASF";
  if (names.includes("ogg")) return "Ogg";
  return (names[0] ?? "").toUpperCase();
}
