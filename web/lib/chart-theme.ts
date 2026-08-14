/**
 * The one place the chart look is defined.
 *
 * uPlot draws to a canvas, which cannot resolve `var(--qz-*)`, so every colour
 * and font here is a literal. Each literal is documented against the token in
 * `globals.css` it must equal — when a token changes, this file changes with
 * it.
 *
 * Both chart consumers ({@link LiveChart} and the analytics page's historical
 * chart) build their uPlot config from {@link baseOptions}, so the two kinds
 * of chart differ only where their behavior has to.
 */

import type uPlot from 'uplot';

/**
 * The accent-derived series palette.
 *
 * Quartz green leads because it is the brand accent and the first series is
 * usually the one being watched. The rest are chosen to stay distinguishable
 * against the dark surface and from each other for the common forms of colour
 * blindness — hue alone is not enough, so they also differ in lightness.
 *
 * The first four equal status tokens:
 *
 *     #00d992 = --qz-accent / --qz-success
 *     #4fb3ff = --qz-info
 *     #f5b243 = --qz-warn
 *     #ff5d6c = --qz-danger
 *
 * The last two (`#b39dff`, `#5ad1c8`) are chart-only extensions of the status
 * palette: charts routinely plot more series than there are statuses, so they
 * have no token equivalent by design.
 */
export const SERIES_COLOURS = ['#00d992', '#4fb3ff', '#f5b243', '#ff5d6c', '#b39dff', '#5ad1c8'];

/**
 * Axis and legend type: the full fallback chain from `--qz-font-mono`, sized
 * for tick text. The `ui-monospace` keyword is left out because canvas font
 * strings do not reliably accept it; the concrete faces behind it are listed
 * instead.
 */
export const CHART_FONT = '11px "JetBrains Mono", "SFMono-Regular", Menlo, Consolas, monospace';

/** Axis chrome. Token-equal literals, since the canvas cannot read tokens. */
export const AXIS = {
  /** Axis line and tick-label colour — equals `--qz-ink-8` (`--qz-fg-4`). */
  stroke: '#6b6f7a',
  /** Gridline colour — equals `--qz-divider`. */
  grid: '#1c1f28',
  /** Tick-mark colour — equals `--qz-ink-5` (`--qz-border`). */
  ticks: '#252830',
} as const;

/**
 * The default plot height, shared by the live and historical charts so two
 * identically-titled Surfaces render the same size panel.
 */
export const CHART_HEIGHT = 220;

/**
 * The translucent area fill under a series line, derived from that series'
 * own stroke so overlapping fills still read by hue. The 0.14 opacity matches
 * Lumen's `LiveGraph`, keeping the consoles' charts identical.
 */
export function seriesFill(index: number): string {
  const colour = SERIES_COLOURS[index % SERIES_COLOURS.length] ?? '#00d992';
  const r = parseInt(colour.slice(1, 3), 16);
  const g = parseInt(colour.slice(3, 5), 16);
  const b = parseInt(colour.slice(5, 7), 16);
  return `rgba(${r}, ${g}, ${b}, 0.14)`;
}

/** One axis carrying the shared stroke, grid, tick, and font treatment. */
export function axisConfig(): uPlot.Axis {
  return {
    stroke: AXIS.stroke,
    grid: { stroke: AXIS.grid, width: 1 },
    ticks: { stroke: AXIS.ticks },
    font: CHART_FONT,
  };
}

/** The per-chart knobs {@link baseOptions} accepts. */
export interface BaseOptionsArgs {
  /** Initial plot width in pixels; resizing afterwards goes through `setSize`. */
  width: number;
  /** Plot height. Defaults to the shared {@link CHART_HEIGHT}. */
  height?: number;
  /** Y-axis label. */
  unit: string;
  /** Formats a Y value for the axis ticks. */
  format: (value: number) => string;
  /**
   * Whether a cursor drag zooms the X scale.
   *
   * Historical charts set this true — zooming into a recorded range is the
   * point of them. Live charts set it false: the next sample's `setData`
   * would snap the zoom straight back out, so offering the gesture would
   * only fight the stream.
   */
  setScale: boolean;
}

/**
 * Everything about a chart's config except its series: dimensions, cursor,
 * legend, scales, and both axes. Callers spread this and add `series`.
 */
export function baseOptions({
  width,
  height = CHART_HEIGHT,
  unit,
  format,
  setScale,
}: BaseOptionsArgs): Omit<uPlot.Options, 'series'> {
  return {
    width,
    height,
    // The Surface head already titles each chart; a second title inside the
    // plot area would only cost vertical space.
    title: '',
    legend: { show: true, live: true },
    cursor: { drag: { x: true, y: false, setScale } },
    scales: { x: { time: true } },
    axes: [
      axisConfig(),
      {
        ...axisConfig(),
        label: unit,
        labelSize: 30,
        labelFont: CHART_FONT,
        size: 64,
        values: (_self, splits) => splits.map((v) => format(v)),
      },
    ],
  };
}
