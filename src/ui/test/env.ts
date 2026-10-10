/**
 * Test-only environment: initializes i18n and fills in browser APIs that jsdom lacks but Radix and the
 * frame renderer rely on. Import it first in every UI test file. Not a test file itself.
 */
import "@/i18n";

type Writable = { [key: string]: unknown };

const globals = globalThis as unknown as Writable;

if (!globals.ResizeObserver) {
  class ResizeObserverStub {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  globals.ResizeObserver = ResizeObserverStub;
}

if (!globals.PointerEvent) {
  class PointerEventStub extends MouseEvent {
    readonly pointerId: number;
    readonly pointerType: string;

    constructor(type: string, init: PointerEventInit = {}) {
      super(type, init);
      this.pointerId = init.pointerId ?? 1;
      this.pointerType = init.pointerType ?? "mouse";
    }
  }
  globals.PointerEvent = PointerEventStub;
}

if (!globals.ImageData) {
  class ImageDataStub {
    readonly data: Uint8ClampedArray;
    readonly width: number;
    readonly height: number;

    constructor(data: Uint8ClampedArray, width: number, height: number) {
      this.data = data;
      this.width = width;
      this.height = height;
    }
  }
  globals.ImageData = ImageDataStub;
}

// jsdom has no 2D canvas. The stub accepts every call the editor makes and draws nothing: unknown methods are
// no-ops, and the few methods that return a value return a harmless one.
function noopContext(canvas: HTMLCanvasElement): object {
  const target: Record<string | symbol, unknown> = {
    canvas,
    putImageData: () => {},
    createPattern: () => null,
    measureText: () => ({ width: 0 }),
  };
  return new Proxy(target, {
    get(record, property) {
      if (property in record) return record[property];
      return () => undefined;
    },
    set(record, property, value) {
      record[property] = value;
      return true;
    },
  });
}
Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
  configurable: true,
  value: function getContext(this: HTMLCanvasElement) {
    return noopContext(this);
  },
});

const proto = Element.prototype as unknown as Writable;
if (typeof proto.hasPointerCapture !== "function") proto.hasPointerCapture = () => false;
if (typeof proto.setPointerCapture !== "function") proto.setPointerCapture = () => {};
if (typeof proto.releasePointerCapture !== "function") proto.releasePointerCapture = () => {};
if (typeof proto.scrollIntoView !== "function") proto.scrollIntoView = () => {};

if (typeof window.matchMedia !== "function") {
  window.matchMedia = (query: string) =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as MediaQueryList;
}
