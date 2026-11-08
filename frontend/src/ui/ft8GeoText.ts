import { getLang } from '../core/i18n';
import { formatLatLon, geoFor } from '../sdr/ft8Geo';

/** Shared with the decode window: callsign countries and the transmitted grid cell centre. */
export function ft8GeoTexts(text: string): { country: string; location: string } {
  const geo = geoFor(text), zh = getLang() === 'zh';
  const sender = geo.sender ? (zh ? geo.sender.zh : geo.sender.name) : '—';
  return {
    country: geo.receiver ? `${sender} → ${zh ? geo.receiver.zh : geo.receiver.name}` : sender,
    location: geo.location ? formatLatLon(geo.location) : '—',
  };
}
