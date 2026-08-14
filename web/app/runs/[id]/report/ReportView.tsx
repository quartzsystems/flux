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

import { Download, Printer } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import Link from 'next/link';
import { usePathname } from 'next/navigation';
import { useMemo, useRef } from 'react';

import { AppShell } from '@/components/AppShell';
import { Alert, Page, PageBody, PageHeader, Skeleton, Surface } from '@/components/ui';
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
      <Page>
        <PageHeader title="Report" subtitle="No run selected" />
        <PageBody>
          <Alert tone="danger">This URL does not name a run.</Alert>
          <div>
            <Link href="/runs/" className="btn btn-secondary btn-sm">
              Back to runs
            </Link>
          </div>
        </PageBody>
      </Page>
    );
  }

  return (
    <Page>
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
            <button type="button" className="btn btn-secondary btn-sm" onClick={print}>
              <Printer size={14} />
              Print
            </button>
            <a className="btn btn-primary btn-sm" href={url} download>
              <Download size={14} />
              Download
            </a>
          </>
        }
      />

      <PageBody>
        {run.data && run.data.results.length === 0 ? (
          <Alert tone="warn">
            This run recorded no results, so the report contains only its configuration and
            outcome.
          </Alert>
        ) : null}

        <Surface padded={run.isLoading}>
          {run.isLoading ? (
            <Skeleton height={520} />
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

        <p className="note">
          The report is a single self-contained HTML file with no scripts and no external
          assets, so it stays readable after it is downloaded or archived. Printing it to PDF
          produces the deliverable.
        </p>
      </PageBody>
    </Page>
  );
}

/** Extracts the run id from `/runs/<id>/report/`. */
function runIdFrom(pathname: string): string {
  const segments = pathname.split('/').filter(Boolean);
  const index = segments.indexOf('runs');
  return index >= 0 ? (segments[index + 1] ?? '') : '';
}
