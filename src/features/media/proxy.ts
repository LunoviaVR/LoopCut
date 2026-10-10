/**
 * Proxy generation state per media token. `mediaProxyStart` runs once per token; its channel events move the
 * state through running (with progress), ready and failed. Components read it with useProxyState.
 */
import { useSyncExternalStore } from "react";
import { messageOf } from "@/features/import/errors";
import i18n from "@/i18n";
import type { ImportedMedia } from "@/ipc/bindings/ImportedMedia";
import { mediaProxyStart } from "@/ipc/commands";

export type ProxyState =
  | { status: "idle" }
  | { status: "running"; value: number }
  | { status: "ready"; url: string | null }
  | { status: "failed"; message: string };

const IDLE: ProxyState = { status: "idle" };

const states = new Map<string, ProxyState>();
const listeners = new Set<() => void>();

function publish(token: string, next: ProxyState): void {
  // A token forgotten while its proxy was still generating must not come back to life.
  if (!states.has(token)) return;
  states.set(token, next);
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Starts the proxy for media that needs one. Calling again for the same token does nothing. */
export function startProxy(media: ImportedMedia): void {
  const token = media.token;
  const current = states.get(token);
  if (current && current.status !== "failed") return;

  states.set(token, { status: "running", value: 0 });
  for (const listener of listeners) listener();

  mediaProxyStart(token, (event) => {
    if (event.kind === "progress") {
      publish(token, { status: "running", value: Math.min(1, Math.max(0, event.value)) });
    } else if (event.kind === "done") {
      publish(token, { status: "ready", url: event.url });
    } else {
      publish(token, { status: "failed", message: event.error.message });
    }
  })
    .then(() => {
      // The command resolves once the job has settled, and its outcome has already come through the channel.
      // A run still marked running at this point never reported a result. An existing proxy URL is still
      // usable; anything else is a failure, not a silent success.
      if (states.get(token)?.status !== "running") return;
      if (media.proxyUrl) publish(token, { status: "ready", url: media.proxyUrl });
      else publish(token, { status: "failed", message: i18n.t("media.proxyIncomplete") });
    })
    .catch((error: unknown) => {
      publish(token, { status: "failed", message: messageOf(error) });
    });
}

/** Drops the proxy state of a released token. */
export function forgetProxy(token: string): void {
  if (!states.delete(token)) return;
  for (const listener of listeners) listener();
}

/** Current proxy state of a token, or idle when none was started. */
export function useProxyState(token: string | null): ProxyState {
  return useSyncExternalStore(subscribe, () => (token ? (states.get(token) ?? IDLE) : IDLE));
}

/** Clears all proxy state. Intended for tests. */
export function resetProxyStates(): void {
  states.clear();
  for (const listener of listeners) listener();
}
