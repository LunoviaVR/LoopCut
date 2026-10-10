import { CommandError } from "@/ipc/commands";

/** Raised when a saved project's media file is no longer at the path the project records. */
export class MissingMediaError extends Error {
  readonly code = "media_missing";
  readonly path: string;

  constructor(path: string) {
    super(`The media for ${path} is no longer open`);
    this.name = "MissingMediaError";
    this.path = path;
  }
}

/** The AppError code of a failure, or `unknown`. */
export function errorCodeOf(error: unknown): string {
  if (error instanceof CommandError) return error.code;
  if (error instanceof MissingMediaError) return error.code;
  return "unknown";
}

/** Human-readable detail of a failure, whatever it was thrown as. */
export function messageOf(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null && "message" in error) {
    const message = (error as { message: unknown }).message;
    if (typeof message === "string") return message;
  }
  return String(error);
}
