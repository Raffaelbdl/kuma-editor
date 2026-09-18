// Driving an overlay document from the editor.
//
// An overlay is a pure function of time, and two panels ask it for a frame: the preview
// stage, where the clip's own time drives it, and the overlay workbench, where you scrub
// the thing on its own. Both scrub it the same way the bake does, so the answer is here
// once rather than once per panel.

type Timeline = { pause?: () => void; time?: (t: number) => void; seek?: (t: number) => void };

/**
 * Seek `iframe`'s overlay document to `t` seconds. The hyperframes runtime's player
 * scrubs every registered GSAP timeline plus Web Animations (CSS keyframes/transitions)
 * exactly as the bake does; until it is up, drive those directly.
 */
export function seekOverlay(iframe: HTMLIFrameElement, t: number): void {
  const win = iframe.contentWindow as (Window & { __player?: { renderSeek?: (t: number) => void }; __timelines?: Record<string, Timeline> }) | null;
  if (!win) return;
  try {
    if (win.__player?.renderSeek) { win.__player.renderSeek(t); return; }
    for (const tl of Object.values(win.__timelines ?? {})) { tl.pause?.(); if (tl.time) tl.time(t); else tl.seek?.(t); }
    for (const a of win.document.getAnimations()) { a.pause(); a.currentTime = t * 1000; }
  } catch { /* cross-origin or not ready */ }
}
