/**
 * The run view route.
 *
 * A static export has to know every path at build time, but run ids are created
 * at run time. The resolution is a single placeholder segment: this route
 * exports one document at `/runs/__id__/`, and `fluxd` serves it for any
 * `/runs/<something>/` that does not match a real file. The client reads the
 * actual id out of `window.location`, which is where the truth is anyway.
 *
 * The alternative — a query parameter like `/runs/detail?id=…` — would work
 * without the server-side mapping but would give up the readable, linkable URLs
 * the rest of the product uses.
 */

import { RunView } from './RunView';

/** The placeholder segment `fluxd` maps unmatched run paths onto. */
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

export default function RunPage() {
  return <RunView />;
}
