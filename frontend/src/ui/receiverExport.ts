import type { DecoderImage, DecoderLogEntry, DecoderRaster } from '../sdr/decoderLog';

const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);

/** A snapshot of retained results, never a replacement for backend continuous recording. */
export function resultExport(entries: readonly DecoderLogEntry[], identity: string, format: 'csv' | 'jsonl'): string {
  if (format === 'jsonl') return entries.filter(entry => !['image', 'status'].includes(entry.kind)).map(entry => JSON.stringify({
    identity, received_at_ms: entry.at, time_basis: 'browser_receipt', source: entry.source,
    mode: entry.mode, provider: entry.provider, kind: entry.kind, timing: entry.timing, data: entry.data,
  })).join('\n') + '\n';
  const csv = (value: unknown): string => {
    const text = String(value ?? '');
    const safe = typeof value === 'string' ? text.replace(/^[=+@\-\t\r]/, c => `'${c}`) : text;
    return `"${safe.replace(/"/g, '""')}"`;
  };
  const rows: unknown[][] = [['identity', 'mode', 'source', 'received_at_utc', 'slot_utc', 'snr_db', 'dt_s', 'audio_hz', 'message']];
  for (const entry of entries) if (entry.kind === 'table' && record(entry.data) && Array.isArray(entry.data.decodes)) {
    for (const row of entry.data.decodes) if (record(row)) rows.push([identity, entry.mode, entry.source, new Date(entry.at).toISOString(),
      row.slot_utc, row.snr_db, row.dt_s, row.audio_hz, row.msg ?? row.message ?? row.text]);
  }
  return '\ufeff' + rows.map(row => row.map(csv).join(',')).join('\r\n') + '\r\n';
}

export function downloadResult(doc: Document, data: Blob, filename: string): void {
  const url = URL.createObjectURL(data), anchor = doc.createElement('a');
  anchor.href = url; anchor.download = filename; anchor.hidden = true;
  doc.body.append(anchor); anchor.click(); anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

export function pictureIsPartial(picture: DecoderImage | DecoderRaster): boolean {
  if (!picture.final) return true;
  if ('raster' in picture) return picture.partial || picture.rows < picture.h || picture.missingRows.length > 0;
  let end = 0;
  for (const strip of [...picture.strips].sort((a, b) => a.y - b.y)) {
    if (strip.y > end) return true;
    end = Math.max(end, strip.y + strip.rows);
  }
  return end < picture.h;
}

/** Use the retained pixels, including neutral bands for missing/unreceived rows. */
export async function pictureExport(doc: Document, entry: DecoderLogEntry): Promise<Blob> {
  const picture = entry.data as DecoderImage | DecoderRaster;
  const height = 'raster' in picture ? picture.h : Math.max(1, ...picture.strips.map(strip => strip.y + strip.rows));
  if (picture.w * height * 4 > 20 * 1024 * 1024) throw new Error('Picture exceeds the 20 MiB PNG export limit');
  const canvas = doc.createElement('canvas'); canvas.width = picture.w; canvas.height = height;
  const context = canvas.getContext('2d'); if (!context) throw new Error('Canvas is unavailable');
  if ('raster' in picture) {
    const pixels = context.createImageData(picture.w, height);
    for (let i = 0; i < picture.w * height; i++) {
      pixels.data[i * 4] = picture.rgb[i * 3]; pixels.data[i * 4 + 1] = picture.rgb[i * 3 + 1];
      pixels.data[i * 4 + 2] = picture.rgb[i * 3 + 2]; pixels.data[i * 4 + 3] = 255;
    }
    context.putImageData(pixels, 0, 0);
  } else {
    context.fillStyle = '#808080'; context.fillRect(0, 0, picture.w, height);
    for (const strip of picture.strips) {
      const image = doc.createElement('img'); image.src = `data:image/png;base64,${strip.png}`;
      await image.decode(); context.drawImage(image, 0, strip.y, picture.w, strip.rows);
    }
  }
  return new Promise((resolve, reject) => canvas.toBlob(blob => blob ? resolve(blob) : reject(new Error('PNG export failed')), 'image/png'));
}
