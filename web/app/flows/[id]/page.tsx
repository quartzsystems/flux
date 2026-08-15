/**
 * The flow editor route.
 *
 * A static export has to know every path at build time, but flow ids are
 * created at run time. The resolution is the same placeholder-segment
 * arrangement the run view uses: this route exports one document at
 * `/flows/__id__/`, and `fluxd` serves it for any `/flows/<something>/` that
 * does not match a real file. The client reads the actual id out of
 * `window.location`; the id `new` means a flow that does not exist yet.
 */

import { FlowEditorView } from './FlowEditorView';

/** The placeholder segment `fluxd` maps unmatched flow paths onto. */
export const DYNAMIC_SEGMENT = '__id__';

/**
 * Declares the one path this route exports.
 *
 * Required by `output: 'export'`. Returning the placeholder rather than a list
 * of real ids is what makes the route work for ids that do not exist yet.
 */
export function generateStaticParams() {
  return [{ id: DYNAMIC_SEGMENT }];
}

export default function FlowPage() {
  return <FlowEditorView />;
}
