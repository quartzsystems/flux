'use client';

/**
 * A uPlot time-series chart fed directly from the statistics stream.
 *
 * The chart owns its own data arrays and calls `setData` itself. React renders
 * the container (and, until the first sample, a quiet placeholder) and then
 * stays out of the way — at one hertz with several charts on screen, going
 * through React state for every sample would spend the frame budget on
 * reconciliation rather than on drawing.
 *
 * uPlot wants columnar data: `[timestamps, series0, series1, …]`, each the same
 * length. The arrays here are mutated in place and sliced to the window.
 */

import uPlot from 'uplot';
import { useEffect, useRef, useState } from 'react';

import type { StatsBatch } from '@/lib/api-types';
import { CHART_HEIGHT, SERIES_COLOURS, baseOptions, seriesFill } from '@/lib/chart-theme';
import type { StatsStream } from '@/lib/stream';

/** How many points to keep on screen. Ten minutes at one hertz. */
const WINDOW = 600;

/** One line on the chart. */
export interface ChartSeries {
  /** Legend label. */
  label: string;
  /** Pulls this series' value out of a sample. Return null for a gap. */
  value: (batch: StatsBatch) => number | null;
}

/** Props for {@link LiveChart}. */
export interface LiveChartProps {
  /** The stream to read from. */
  stream: StatsStream;
  /** Series to plot, in legend order. */
  series: ChartSeries[];
  /** Y-axis label. */
  unit: string;
  /** Formats a Y value for the axis and cursor. */
  format: (value: number) => string;
  /** Chart height in pixels. Defaults to the shared {@link CHART_HEIGHT}. */
  height?: number;
}

/** Renders a live-updating line chart. */
export function LiveChart({ stream, series, unit, format, height = CHART_HEIGHT }: LiveChartProps) {
  const container = useRef<HTMLDivElement>(null);
  const chart = useRef<uPlot | null>(null);

  // Whether the quiet placeholder still shows. This is the one piece of React
  // state here: it changes at most once per mount (empty → drawing), so it
  // does not drag rendering back through React at the stream's cadence.
  const [waiting, setWaiting] = useState(() => stream.buffer.current.length === 0);

  // Column-oriented, matching uPlot's expected shape: index 0 is time.
  const data = useRef<number[][]>([]);

  // Kept in a ref so the effect below does not re-create the chart when a
  // caller passes a new inline array or closure on every render.
  const config = useRef({ series, format, unit });
  config.current = { series, format, unit };

  useEffect(() => {
    const element = container.current;
    if (!element) return;

    data.current = Array.from({ length: config.current.series.length + 1 }, () => []);

    const build = (width: number) =>
      new uPlot(
        {
          ...baseOptions({
            width,
            height,
            unit: config.current.unit,
            format: (v) => config.current.format(v),
            // Off for live charts: the next sample's setData would snap a
            // drag-zoom straight back out (see BaseOptionsArgs).
            setScale: false,
          }),
          series: [
            { label: 'time' },
            ...config.current.series.map((s, i) => ({
              label: s.label,
              stroke: SERIES_COLOURS[i % SERIES_COLOURS.length],
              width: 1.5,
              // Every series carries a translucent fill of its own colour,
              // matching Lumen's LiveGraph, so overlapping areas still read
              // by hue.
              fill: seriesFill(i),
              value: (_self: uPlot, v: number | null) =>
                v === null ? '—' : config.current.format(v),
            })),
          ],
        },
        data.current as unknown as uPlot.AlignedData,
        element,
      );

    chart.current = build(element.clientWidth || 600);

    // uPlot does not resize itself; without this the chart keeps the width it
    // had when the sidebar was expanded.
    const observer = new ResizeObserver((entries) => {
      const width = entries[0]?.contentRect.width;
      if (width && chart.current) {
        chart.current.setSize({ width, height });
      }
    });
    observer.observe(element);

    // Backfill whatever the stream already holds, so a chart mounted mid-run
    // renders full rather than drawing itself in from the right.
    for (const batch of stream.buffer.current) {
      push(data.current, batch, config.current.series);
    }
    chart.current.setData(data.current as unknown as uPlot.AlignedData);
    setWaiting((data.current[0]?.length ?? 0) === 0);

    const unsubscribe = stream.subscribe((batch) => {
      push(data.current, batch, config.current.series);
      chart.current?.setData(data.current as unknown as uPlot.AlignedData);
      // A no-op after the first sample; React bails out on identical state.
      setWaiting(false);
    });

    return () => {
      unsubscribe();
      observer.disconnect();
      chart.current?.destroy();
      chart.current = null;
    };
    // `series` is intentionally excluded: it is read through a ref, and
    // including it would tear down the chart whenever a parent re-rendered.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stream, height]);

  return (
    <div
      ref={container}
      className="live-chart"
      // The plot's height is reserved before the first sample arrives so the
      // panel does not jump when the chart starts drawing. min-height rather
      // than height because uPlot appends its legend below the plot.
      style={{ position: 'relative', minHeight: height }}
    >
      {waiting ? (
        <div
          className="muted"
          style={{
            position: 'absolute',
            inset: 0,
            display: 'grid',
            placeItems: 'center',
            pointerEvents: 'none',
          }}
        >
          Waiting for samples…
        </div>
      ) : null}
    </div>
  );
}

/**
 * Appends one sample and trims to the window.
 *
 * Every column is appended to unconditionally, including with `null`, because
 * uPlot requires all series to be the same length as the time column.
 */
function push(data: number[][], batch: StatsBatch, series: ChartSeries[]) {
  const time = data[0];
  if (!time) return;

  time.push(batch.ts);

  for (let i = 0; i < series.length; i += 1) {
    const column = data[i + 1];
    const extract = series[i];
    if (!column || !extract) continue;
    // uPlot reads null as a gap in the line, which is the honest rendering for
    // a series that reported nothing this second.
    column.push(extract.value(batch) as number);
  }

  if (time.length > WINDOW) {
    const excess = time.length - WINDOW;
    for (const column of data) {
      column.splice(0, excess);
    }
  }
}
