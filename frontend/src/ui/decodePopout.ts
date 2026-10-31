import { t } from '../core/i18n';
import { createSurfacePortal, type SurfacePortal } from './surfacePortal';

export type DecodePopout = SurfacePortal;

/** Compatibility wrapper; the shared portal has one sync callback per instance. */
export function createDecodePopout(root: HTMLElement, beforeMove: () => void, afterMove: () => void): DecodePopout {
  return createSurfacePortal(root, { title: () => t('decode_popup_title'), beforeMove, afterMove,
    popSelector: '#btn-decode-popout', dockSelector: '#btn-decode-dock', errorSelector: '#decode-popout-error' });
}
