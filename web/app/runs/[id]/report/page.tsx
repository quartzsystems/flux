/**
 * The report route.
 *
 * Like the run view, this exports one document under a placeholder segment and
 * `fluxd` maps `/runs/<id>/report/` onto it. The nested route resolves before
 * the parent, so a report link never lands on the run view.
 */

import { DYNAMIC_SEGMENT } from '../page';
import { ReportView } from './ReportView';

/** Declares the one path this route exports. */
export function generateStaticParams() {
  return [{ id: DYNAMIC_SEGMENT }];
}

export default function ReportPage() {
  return <ReportView />;
}
