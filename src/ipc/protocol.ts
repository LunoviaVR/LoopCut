import { convertFileSrc } from "@tauri-apps/api/core";

/** Custom URI schemes served by the Rust side (see src-tauri/src/protocol). */
export type LoopcutScheme = "loopcut" | "loopcut-media";

/**
 * Builds a URL for one of LoopCut's custom protocols.
 *
 * On Windows Tauri serves custom schemes as `http://<scheme>.localhost/<path>`; elsewhere as
 * `<scheme>://localhost/<path>`. `convertFileSrc` encodes that rule, so every protocol URL in the app
 * must come from here. Paths are opaque tokens minted by Rust, never filesystem paths.
 */
export function protocolUrl(scheme: LoopcutScheme, path: string): string {
  return convertFileSrc(path, scheme);
}
