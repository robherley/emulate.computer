import { useEffect, useRef, type RefObject, type MutableRefObject } from "react";
import type { EmulatorClient } from "../client";
import { DisplayBuffer } from "../display-buffer";
import { bindDisplayInput } from "../display-input";
import { FB_WIDTH, FB_HEIGHT } from "../protocol";

export type FrameSink = (
  rows: Uint32Array,
  rgba: Uint8ClampedArray<ArrayBuffer>,
  width: number,
  height: number,
) => void;

export function DisplayPanel({
  client,
  frames,
  viewport,
  interactive,
  visible,
}: {
  client: EmulatorClient;
  frames: MutableRefObject<FrameSink>;
  viewport: RefObject<HTMLElement | null>;
  interactive: boolean;
  visible: boolean;
}) {
  const host = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement | null>(null);
  useEffect(() => {
    // A transferred canvas cannot be reused by Strict Mode's next setup.
    const surface = document.createElement("canvas");
    surface.width = FB_WIDTH;
    surface.height = FB_HEIGHT;
    surface.className = "display-canvas";
    surface.tabIndex = 0;
    surface.setAttribute(
      "aria-label",
      "Guest desktop. Click to use its keyboard and pointer.",
    );
    canvas.current = surface;
    host.current!.append(surface);
    const debug = new URLSearchParams(location.search).has("debug");
    let paint = 0;
    if (typeof surface.transferControlToOffscreen === "function") {
      const offscreen = surface.transferControlToOffscreen();
      client.send({ type: "display-attach", canvas: offscreen, debug }, [
        offscreen,
      ]);
    } else {
      client.send({ type: "display-attach", canvas: null, debug });
      const context = surface.getContext("2d", { alpha: false });
      let buffer = new DisplayBuffer(FB_WIDTH, FB_HEIGHT);
      let image = new ImageData(buffer.pixels, FB_WIDTH, FB_HEIGHT);
      surface.addEventListener("contextrestored", () => {
        context?.putImageData(image, 0, 0);
        if (buffer.takeRows().length) client.send({ type: "display-frame-ack" });
      });
      frames.current = (rows, rgba, width, height) => {
        if (buffer.width !== width || buffer.height !== height) {
          surface.width = width; surface.height = height;
          buffer = new DisplayBuffer(width, height);
          image = new ImageData(buffer.pixels, width, height);
        }
        buffer.update(rows, rgba);
        if (paint) return;
        paint = requestAnimationFrame(() => {
          paint = 0;
          if (context?.isContextLost?.()) return;
          const changed = buffer.takeRows();
          if (!changed.length) return;
          const first = changed[0], last = changed[changed.length - 1];
          context?.putImageData(image, 0, 0, 0, first, buffer.width, last - first + 1);
          client.send({ type: "display-frame-ack" });
        });
      };
    }
    const unbind = bindDisplayInput(surface, (event) =>
      client.send({ type: "display-input", event }),
    );
    let resizeTimer: ReturnType<typeof setTimeout>;
    const observer = new ResizeObserver(() => {
      const { width, height } = viewport.current!.getBoundingClientRect();
      if (width < 1 || height < 1) return;
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => client.send({ type: "display-resize",
        width: Math.max(320, Math.min(2048, Math.floor(width))),
        height: Math.max(200, Math.min(2048, Math.floor(height))),
      }), 100);
    });
    observer.observe(viewport.current!);
    return () => {
      cancelAnimationFrame(paint);
      observer.disconnect();
      clearTimeout(resizeTimer);
      unbind();
      frames.current = () => {};
      client.send({ type: "display-visible", visible: false });
      surface.remove();
      canvas.current = null;
    };
  }, [client, frames, viewport]);
  useEffect(() => {
    const update = () =>
      client.send({ type: "display-visible", visible: visible && !document.hidden });
    update();
    document.addEventListener("visibilitychange", update);
    return () => document.removeEventListener("visibilitychange", update);
  }, [client, visible]);
  useEffect(() => {
    if (canvas.current) canvas.current.tabIndex = interactive ? 0 : -1;
    if (interactive && document.activeElement === document.body)
      canvas.current?.focus();
  }, [interactive]);
  return <div ref={host} className="display-surface" />;
}
