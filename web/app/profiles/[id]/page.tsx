/**
 * The load profile editor route.
 *
 * The same placeholder-segment arrangement the run and flow views use: this
 * route exports one document at `/profiles/__id__/`, and `fluxd` serves it for
 * any `/profiles/<something>/` that does not match a real file. The client
 * reads the actual id out of `window.location`; the id `new` means a profile
 * that does not exist yet.
 */

import { ProfileEditorView } from './ProfileEditorView';

/** The placeholder segment `fluxd` maps unmatched profile paths onto. */
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

export default function ProfilePage() {
  return <ProfileEditorView />;
}
