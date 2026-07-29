'use client';

/**
 * The live statistics WebSocket.
 *
 * The important property of this hook is what it does *not* do: incoming samples
 * never become React state. At one hertz, with charts holding six hundred
 * points, re-rendering the tree on every sample would spend the whole budget on
 * reconciliation. Instead samples land in a mutable buffer and subscribers —
 * the uPlot instances — are called directly.
 *
 * React state is used only for things that change rarely: the connection status
 * and the run progress header.
 */

import { useCallback, useEffect, useRef, useState } from 'react';

import { statsBatchSchema, streamControlSchema, type RunProgress, type StatsBatch } from './api-types';

/** How long to wait before the first reconnection attempt. */
const INITIAL_RECONNECT_MS = 1_000;

/** Longest wait between reconnection attempts. */
const MAX_RECONNECT_MS = 15_000;

/** How many samples to retain. Matches the collector's ring buffer. */
const BUFFER_DEPTH = 600;

/** Where the connection is. */
export type StreamStatus = 'connecting' | 'open' | 'closed' | 'error';

/** A callback invoked for every sample, outside the React render cycle. */
export type SampleListener = (batch: StatsBatch) => void;

/** What `useStatsStream` returns. */
export interface StatsStream {
  /** Connection state, for the indicator in the header. */
  status: StreamStatus;
  /** The most recent run progress, or null when the stream carries none. */
  progress: RunProgress | null;
  /** Every sample received this session, oldest first. */
  buffer: React.RefObject<StatsBatch[]>;
  /** Registers a listener; returns a function that removes it. */
  subscribe: (listener: SampleListener) => () => void;
  /** A message explaining a closed or errored connection. */
  detail: string | null;
}

/**
 * Opens the statistics stream and delivers samples to subscribers.
 *
 * `selectors` is the subscription sent on connect. It is captured per
 * connection, so changing it reconnects — which is what should happen, since the
 * server filters by it.
 */
export function useStatsStream(selectors: string[], enabled = true): StatsStream {
  const [status, setStatus] = useState<StreamStatus>(enabled ? 'connecting' : 'closed');
  const [progress, setProgress] = useState<RunProgress | null>(null);
  const [detail, setDetail] = useState<string | null>(null);

  const buffer = useRef<StatsBatch[]>([]);
  const listeners = useRef<Set<SampleListener>>(new Set());
  const socket = useRef<WebSocket | null>(null);
  const reconnectDelay = useRef(INITIAL_RECONNECT_MS);
  const reconnectTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const closedByUs = useRef(false);

  // Selectors are compared by value, not identity: a caller building the array
  // inline would otherwise reconnect on every render.
  const key = selectors.join('|');

  const subscribe = useCallback((listener: SampleListener) => {
    listeners.current.add(listener);
    return () => {
      listeners.current.delete(listener);
    };
  }, []);

  useEffect(() => {
    if (!enabled) {
      setStatus('closed');
      return;
    }

    closedByUs.current = false;
    let disposed = false;

    const connect = () => {
      if (disposed) return;

      setStatus('connecting');

      // Same origin as the page: in production fluxd serves both, and in
      // development the Next dev server proxies /api through to it.
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const ws = new WebSocket(`${protocol}//${window.location.host}/api/v1/stream`);
      socket.current = ws;

      ws.onopen = () => {
        if (disposed) return;
        reconnectDelay.current = INITIAL_RECONNECT_MS;
        setStatus('open');
        setDetail(null);
        ws.send(JSON.stringify({ subscribe: key.split('|') }));
      };

      ws.onmessage = (event) => {
        if (disposed || typeof event.data !== 'string') return;

        let parsed: unknown;
        try {
          parsed = JSON.parse(event.data);
        } catch {
          return;
        }

        // Control frames carry a `type`; batches do not. Checking the control
        // schema first keeps the hot path — batches — to one parse.
        const control = streamControlSchema.safeParse(parsed);
        if (control.success) {
          if (control.data.type === 'error') {
            setDetail(control.data.message);
          }
          return;
        }

        const batch = statsBatchSchema.safeParse(parsed);
        if (!batch.success) {
          console.error('Unexpected stream payload', batch.error.issues);
          return;
        }

        // Straight into the buffer and out to the charts. Deliberately not
        // state: this runs once a second and must not re-render the tree.
        buffer.current.push(batch.data);
        if (buffer.current.length > BUFFER_DEPTH) {
          buffer.current.splice(0, buffer.current.length - BUFFER_DEPTH);
        }
        for (const listener of listeners.current) {
          listener(batch.data);
        }

        // Progress is the exception: it drives a header that has to re-render,
        // and it changes at most once a second.
        if (batch.data.run) {
          setProgress((previous) =>
            sameProgress(previous, batch.data.run ?? null) ? previous : batch.data.run ?? null,
          );
        }
      };

      ws.onerror = () => {
        if (!disposed) setStatus('error');
      };

      ws.onclose = () => {
        if (disposed || closedByUs.current) return;
        setStatus('closed');

        // Back off so a daemon restart does not get hammered while it comes up.
        reconnectTimer.current = setTimeout(connect, reconnectDelay.current);
        reconnectDelay.current = Math.min(reconnectDelay.current * 2, MAX_RECONNECT_MS);
      };
    };

    connect();

    return () => {
      disposed = true;
      closedByUs.current = true;
      if (reconnectTimer.current) clearTimeout(reconnectTimer.current);
      socket.current?.close();
      socket.current = null;
    };
  }, [key, enabled]);

  return { status, progress, buffer, subscribe, detail };
}

/**
 * Whether two progress objects say the same thing.
 *
 * Used to avoid a state update — and therefore a re-render — when a run is
 * sitting in the same state second after second, which is most of a run.
 */
function sameProgress(a: RunProgress | null, b: RunProgress | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.runId === b.runId &&
    a.state === b.state &&
    a.iteration === b.iteration &&
    a.frameSize === b.frameSize &&
    a.trialRatePct === b.trialRatePct &&
    a.progress === b.progress &&
    a.message === b.message
  );
}
