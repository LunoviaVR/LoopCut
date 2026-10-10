/**
 * Output frame plan for a project, mirroring `sequencer::plan` in crates/loopcut-core/src/sequencer.rs.
 *
 * The IPC `sequence_plan` command returns only a summary (frame count, duration, delays). The timeline needs the
 * source time every output frame shows, so the frontend builds the same plan from the project. Keep this file in
 * step with the Rust sequencer; the frame count is compared with `sequence_plan` at runtime (useSequencePlanSync).
 * Pure functions only, no state.
 */
import type { Clip } from "@/ipc/bindings/Clip";
import type { PlannedFrame } from "@/ipc/bindings/PlannedFrame";
import type { Project } from "@/ipc/bindings/Project";
import type { Sample } from "@/ipc/bindings/Sample";
import type { SequencePlan } from "@/ipc/bindings/SequencePlan";
import { clamp } from "./numeric";
import { gifDelaysCs, isValidRational, toFloat } from "./time";

/** Speed range the sequencer clamps to (`sequencer::MIN_SPEED` and `MAX_SPEED`). */
export const SEQUENCE_MIN_SPEED = 0.25;
export const SEQUENCE_MAX_SPEED = 4;
/** Hard frame cap (`sequencer::MAX_OUTPUT_FRAMES`). */
export const MAX_OUTPUT_FRAMES = 3000;
/** Highest output frame rate the sequencer accepts. */
const MAX_PLAN_FPS = 240;

type Piece =
  | { kind: "play"; start: number; end: number }
  | { kind: "hold"; at: number; duration: number };

function pieceDuration(piece: Piece, speed: number): number {
  return piece.kind === "play" ? (piece.end - piece.start) / speed : piece.duration;
}

function pieceSourceTime(piece: Piece, localOut: number, speed: number): number {
  return piece.kind === "play" ? Math.min(piece.start + localOut * speed, piece.end) : piece.at;
}

/** Play and hold pieces of a clip in order, or null when the clip has no valid segment. */
function piecesOf(clip: Clip): Piece[] | null {
  if (clip.segments.length === 0) return null;
  const segments = [...clip.segments].sort((a, b) => a.start - b.start);
  const holds = clip.holds
    .filter((hold) => hold.duration > 0 && Number.isFinite(hold.at))
    .sort((a, b) => a.at - b.at);
  const out: Piece[] = [];
  for (const segment of segments) {
    const valid =
      Number.isFinite(segment.start) &&
      Number.isFinite(segment.end) &&
      segment.end > segment.start &&
      segment.start >= 0;
    if (!valid) return null;
    let cursor = segment.start;
    for (const hold of holds) {
      if (hold.at < segment.start || hold.at >= segment.end) continue;
      if (hold.at > cursor) out.push({ kind: "play", start: cursor, end: hold.at });
      out.push({ kind: "hold", at: hold.at, duration: hold.duration });
      cursor = hold.at;
    }
    if (segment.end > cursor) out.push({ kind: "play", start: cursor, end: segment.end });
  }
  return out;
}

const sampleTime = (sample: Sample): number => (sample.kind === "at" ? sample.t : sample.a);

/**
 * The plan for the project's first clip (v1 edits one clip), or null when the project cannot be planned: no
 * valid segment, an invalid frame rate, or more output frames than the sequencer allows.
 */
export function buildSequencePlan(project: Project): SequencePlan | null {
  const clip = project.clips[0];
  const fps = project.output.fps;
  if (!clip || !isValidRational(fps) || toFloat(fps) > MAX_PLAN_FPS) return null;

  const { timing } = project;
  const speed = Number.isFinite(timing.speed)
    ? clamp(timing.speed, SEQUENCE_MIN_SPEED, SEQUENCE_MAX_SPEED)
    : 1;
  const pieces = piecesOf(clip);
  if (!pieces || pieces.length === 0) return null;

  const total = pieces.reduce((sum, piece) => sum + pieceDuration(piece, speed), 0);
  const fpsValue = toFloat(fps);
  const count = Math.max(1, Math.ceil(total * fpsValue - 1e-6));
  if (count > MAX_OUTPUT_FRAMES) return null;

  // Forward samples: output frame k shows the source at output time k / fps.
  const forward: number[] = [];
  let pieceIndex = 0;
  let pieceStart = 0;
  for (let k = 0; k < count; k++) {
    const tOut = k / fpsValue;
    while (
      pieceIndex + 1 < pieces.length &&
      tOut >= pieceStart + pieceDuration(pieces[pieceIndex] as Piece, speed) - 1e-9
    ) {
      pieceStart += pieceDuration(pieces[pieceIndex] as Piece, speed);
      pieceIndex += 1;
    }
    const piece = pieces[pieceIndex];
    if (!piece) return null;
    forward.push(pieceSourceTime(piece, tOut - pieceStart, speed));
  }

  const at = (t: number): Sample => ({ kind: "at", t });
  let samples: Sample[];
  if (timing.direction === "forward") {
    samples = forward.map(at);
  } else if (timing.direction === "reverse") {
    samples = [...forward].reverse().map(at);
  } else {
    samples = forward.map(at);
    if (forward.length > 2) {
      samples.push(...forward.slice(1, -1).reverse().map(at));
    }
  }

  // Seamless-loop crossfade. Ping-pong already loops seamlessly, so it never blends.
  if (timing.direction !== "pingPong" && timing.crossfade > 0) {
    const n = samples.length;
    const blendFrames = Math.min(Math.round(timing.crossfade * fpsValue), Math.floor((n - 1) / 2));
    if (blendFrames > 0) {
      const blended: Sample[] = [];
      for (let i = 0; i < blendFrames; i++) {
        const tail = samples[n - blendFrames + i];
        const head = samples[i];
        if (!tail || !head) return null;
        blended.push({
          kind: "blend",
          a: sampleTime(tail),
          b: sampleTime(head),
          weightB: (i + 1) / (blendFrames + 1),
        });
      }
      blended.push(...samples.slice(blendFrames, n - blendFrames));
      samples = blended;
    }
  }

  const delays = gifDelaysCs(samples.length, fps);
  const frames: PlannedFrame[] = samples.map((sample, index) => ({
    sample,
    delayCs: delays[index] ?? 0,
  }));
  return {
    fps: { num: fps.num, den: fps.den },
    frames,
    duration: frames.length / fpsValue,
  };
}
