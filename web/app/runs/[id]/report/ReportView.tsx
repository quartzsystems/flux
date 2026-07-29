'use client';

/**
 * The report page.
 *
 * The report itself is rendered by `fluxd` into one self-contained HTML
 * document; this page frames it. That split is deliberate: the document has to
 * stand on its own when it is downloaded, emailed, or opened years later, and
 * duplicating the layout in React would give two renderings that drift apart.
 *
 * The frame is an iframe, and printing targets the frame rather than the page,
 * so `window.print()` produces the document with its own print stylesheet and
 * none of the application shell.
 */

import { IconDownload, IconPrinter } from '@tabler/icons-react';
import { useQuery } from '@tanstack/react-query';
import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { useMemo, useRef } from 'react';

import { AppShell } from '@/components/AppShell';
import { Alert, PageHeader, Skeleton, Surface } from '@/components/ui';
import { api, queryKeys } from '@/lib/api';
import { formatTimestamp } from '@/lib/format';

export function ReportView() {
  return (
    <AppShell>
      <Report />
    </AppShell>
  );
}

function Report() {
  const pathname = usePathname();
  const runId = useMemo(() => runIdFrom(pathname), [pathname]);
  const frame = useRef<HTMLIFrameElement>(null);

  const run = useQuery({
    queryKey: queryKeys.run(runId),
    queryFn: ({ signal }) => api.runs.get(runId, signal),
    enabled: runId !== '',
  });

  const url = api.runs.reportUrl(runId);

  /**
   * Prints the report rather than the page.
   *
   * Targeting the frame's own window is what makes the output the document with
   * its print stylesheet, instead of the application shell around it.
   */
  const print = () => {
    const window = frame.current?.contentWindow;
    if (window) {
      window.focus();
      window.print();
    }
  };

  if (runId === '') {
    return (
      <div className="page stack gap-18">
        <PageHeader title="Report" subtitle="No run selected" />
        <Alert tone="danger">
          <span>This URL does not name a run.</span>
        </Alert>
      </div>
    );
  }

  return (
    <div className="page stack gap-18">
      <PageHeader
        title={run.data ? `${run.data.testName} — report` : 'Report'}
        subtitle={
          run.data ? (
            <>
              {run.data.type} · {formatTimestamp(run.data.startedAt)}
            </>
          ) : (
            'Printable run report'
          )
        }
        actions={
          <>
            <Link href={`/runs/${runId}/`} className="btn btn-secondary btn-sm">
              Back to run
            </Link>
            <a className="btn btn-secondary btn-sm" href={url} download>
              <IconDownload size={14} stroke={1.8} />
              Download
            </a>
            <button type="button" className="btn btn-primary btn-sm" onClick={print}>
              <IconPrinter size={14} stroke={1.8} />
              Print
            </button>
          </>
        }
      />

      {run.data && run.data.results.length === 0 ? (
        <Alert tone="warn">
          <span>
            This run recorded no results, so the report contains only its configuration and
            outcome.
          </span>
        </Alert>
      ) : null}

      <Surface padded={false}>
        {run.isLoading ? (
          <div style={{ padding: 18 }}>
            <Skeleton height={520} />
          </div>
        ) : (
          // The document renders on white; the surrounding surface stays dark,
          // so what is on screen is what will come out of the printer.
          <iframe
            ref={frame}
            src={url}
            title="Run report"
            className="report-frame"
            // The report is same-origin and carries no script, but the sandbox
            // is set anyway: this frame renders operator-supplied text, and a
            // narrower capability set costs nothing here.
            sandbox="allow-same-origin allow-modals"
          />
        )}
      </Surface>

      <p className="dim" style={{ margin: 0, fontSize: 12.5 }}>
        The report is a single self-contained HTML file with no scripts and no external
        assets, so it stays readable after it is downloaded or archived. Printing it to PDF
        produces the deliverable.
      </p>
    </div>
  );
}

/** Extracts the run id from `/runs/<id>/report/`. */
function runIdFrom(pathname: string): string {
  const segments = pathname.split('/').filter(Boolean);
  const index = segments.indexOf('runs');
  return index >= 0 ? (segments[index + 1] ?? '') : '';
}
