import { Channel, invoke } from "@tauri-apps/api/core";
import { protocolUrl } from "@/ipc/protocol";

/** Webview environment facts that decide the preview architecture (spike a/b). */
export interface WebviewEnvironment {
  origin: string;
  secureContext: boolean;
  userAgent: string;
  webgpu: { available: boolean; adapter: string | null; error: string | null };
  webcodecs: boolean;
}

export interface TransportResult {
  method: string;
  width: number;
  height: number;
  frames: number;
  ms: number;
  fps: number;
  mbPerSec: number;
  errors: number;
  note?: string;
}

export async function probeEnvironment(): Promise<WebviewEnvironment> {
  const env: WebviewEnvironment = {
    origin: location.origin,
    secureContext: window.isSecureContext,
    userAgent: navigator.userAgent,
    webgpu: { available: "gpu" in navigator, adapter: null, error: null },
    webcodecs: typeof (globalThis as { VideoDecoder?: unknown }).VideoDecoder === "function",
  };
  const gpu = (navigator as Navigator & { gpu?: { requestAdapter(): Promise<unknown> } }).gpu;
  if (gpu) {
    try {
      const adapter = (await gpu.requestAdapter()) as {
        info?: { vendor?: string; architecture?: string; description?: string };
      } | null;
      env.webgpu.adapter = adapter
        ? JSON.stringify({
            vendor: adapter.info?.vendor,
            architecture: adapter.info?.architecture,
            description: adapter.info?.description,
          })
        : null;
    } catch (e) {
      env.webgpu.error = String(e);
    }
  }
  return env;
}

function drawRgba(ctx: CanvasRenderingContext2D, buf: ArrayBuffer, width: number, height: number) {
  ctx.putImageData(new ImageData(new Uint8ClampedArray(buf), width, height), 0, 0);
}

function result(
  method: string,
  width: number,
  height: number,
  frames: number,
  ms: number,
  errors: number,
  note?: string,
): TransportResult {
  const bytes = width * height * 4 * frames;
  return {
    method,
    width,
    height,
    frames,
    ms: Math.round(ms),
    fps: Math.round((frames / ms) * 1000 * 10) / 10,
    mbPerSec: Math.round((bytes / 1024 / 1024 / ms) * 1000 * 10) / 10,
    errors,
    note,
  };
}

/** Rust pushes frames through a `Channel<InvokeResponseBody>`; each is drawn on arrival. */
export async function benchChannel(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  count: number,
): Promise<TransportResult> {
  let received = 0;
  let errors = 0;
  const channel = new Channel<ArrayBuffer>();
  channel.onmessage = (msg) => {
    if (msg instanceof ArrayBuffer && msg.byteLength === width * height * 4) {
      drawRgba(ctx, msg, width, height);
    } else {
      errors++;
    }
    received++;
  };
  const t0 = performance.now();
  await invoke("bench_channel", { width, height, count, onFrame: channel });
  while (received < count && performance.now() - t0 < 30_000) {
    await new Promise((r) => setTimeout(r, 1));
  }
  return result("channel-push", width, height, received, performance.now() - t0, errors);
}

/** The webview pulls frames from the `loopcut` custom protocol with `inFlight` concurrent requests. */
export async function benchProtocol(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  count: number,
  inFlight: number,
): Promise<TransportResult> {
  let errors = 0;
  let done = 0;
  let next = 0;
  const t0 = performance.now();
  const worker = async () => {
    while (next < count) {
      const i = next++;
      try {
        const res = await fetch(protocolUrl("loopcut", `bench/${width}/${height}/${i}`));
        const buf = await res.arrayBuffer();
        if (!res.ok || buf.byteLength !== width * height * 4) errors++;
        else drawRgba(ctx, buf, width, height);
      } catch {
        errors++;
      }
      done++;
    }
  };
  await Promise.all(Array.from({ length: inFlight }, worker));
  return result(`protocol-pull x${inFlight}`, width, height, done, performance.now() - t0, errors);
}

/** Request/response latency for a single frame via `invoke` returning `tauri::ipc::Response`. */
export async function benchInvoke(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  count: number,
): Promise<TransportResult> {
  let errors = 0;
  const lat: number[] = [];
  const t0 = performance.now();
  for (let i = 0; i < count; i++) {
    const s = performance.now();
    const buf = await invoke<ArrayBuffer>("bench_invoke_frame", { width, height, index: i });
    lat.push(performance.now() - s);
    if (!(buf instanceof ArrayBuffer) || buf.byteLength !== width * height * 4) errors++;
    else drawRgba(ctx, buf, width, height);
  }
  lat.sort((a, b) => a - b);
  const p50 = lat[Math.floor(lat.length / 2)] ?? 0;
  const p95 = lat[Math.floor(lat.length * 0.95)] ?? 0;
  return result(
    "invoke-response",
    width,
    height,
    count,
    performance.now() - t0,
    errors,
    `latency p50 ${p50.toFixed(1)} ms, p95 ${p95.toFixed(1)} ms`,
  );
}

/** Runs every spike and writes the report through Rust. Returns the report path. */
export async function runSpikes(canvas: HTMLCanvasElement): Promise<string> {
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no 2d context");
  const report: {
    environment: WebviewEnvironment;
    transport: TransportResult[];
    errors: string[];
  } = {
    environment: await probeEnvironment(),
    transport: [],
    errors: [],
  };
  const sizes: Array<[number, number]> = [
    [1280, 720],
    [1920, 1080],
    [960, 540],
  ];
  for (const [w, h] of sizes) {
    canvas.width = w;
    canvas.height = h;
    for (const run of [
      () => benchChannel(ctx, w, h, 90),
      () => benchProtocol(ctx, w, h, 90, 1),
      () => benchProtocol(ctx, w, h, 90, 3),
      () => benchInvoke(ctx, w, h, 40),
    ]) {
      try {
        report.transport.push(await run());
      } catch (e) {
        report.errors.push(String(e));
      }
    }
  }
  return invoke<string>("spike_report", { json: JSON.stringify(report, null, 2) });
}
