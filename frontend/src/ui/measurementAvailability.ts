import * as S from '../core/store';
import { currentGraphMode, deviceMode, measurementOwnsDevice } from './graphMode';

export function measurementsAvailable(): boolean {
  const reported = deviceMode.get();
  return currentGraphMode() === 'std' && !S.rtaMode && !S.sdrMode
    && reported !== 'rta' && reported !== 'sdr'
    && (!measurementOwnsDevice() || S.measOn);
}

export function sweptMeasurementsAvailable(): boolean {
  return measurementsAvailable() && S.viewMode === 'std';
}
