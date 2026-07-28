/**
 * The Flux logo.
 *
 * Both marks live as static SVG under `public/brand/` and are rendered through
 * plain `<img>`: they are fixed artwork, not content, and `next/image` would add
 * an optimiser this appliance does not run. Width is the only knob — the aspect
 * ratio is fixed by the artwork and must not be overridden.
 */

/** The product tagline. Used verbatim wherever Flux introduces itself. */
export const TAGLINE = 'Traffic generation and load testing that moves at line rate.';

/** Mark plus wordmark, for dark backgrounds. */
export function FluxLockup({ width = 110 }: { width?: number }) {
  return (
    // eslint-disable-next-line @next/next/no-img-element -- static brand artwork, not optimisable content
    <img
      src="/brand/flux-lockup.svg"
      alt="Flux"
      width={width}
      height={Math.round((width * 100) / 300)}
      style={{ display: 'block' }}
    />
  );
}

/** The circular flow mark alone, for collapsed and compact contexts. */
export function FluxMark({ size = 28 }: { size?: number }) {
  return (
    // eslint-disable-next-line @next/next/no-img-element -- static brand artwork, not optimisable content
    <img
      src="/brand/flux-mark.svg"
      alt="Flux"
      width={size}
      height={size}
      style={{ display: 'block' }}
    />
  );
}
