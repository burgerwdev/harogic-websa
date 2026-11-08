import { afterEach, expect, it, vi } from 'vitest';
import { copyDetails } from '../ui/copyDetails';
import { addDecoderResult, clearDecoderLog } from '../sdr/decoderLog';
import { renderDecoderResults } from '../ui/decoderResults';
import { setLang } from '../core/i18n';

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); document.body.replaceChildren(); clearDecoderLog(1); });

it('copies the latest original snapshot without including controls or escaped HTML', async () => {
  setLang('en'); const writeText = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal('navigator', { clipboard: { writeText } });
  const host = document.createElement('div'); document.body.append(host);
  const draw = (callsign: string): void => {
    addDecoderResult({ provider:'onair',mode:'adsb',kind:'table',data:{type:'aircraft',aircraft:[{icao:'ABC123',callsign}]} },'1:1',1);
    renderDecoderResults(host,1,'adsb',false);
  };
  draw('OLD'); const button = host.querySelector<HTMLButtonElement>('[data-action=copy-raw]')!;
  draw('<NEW & 飞行>'); expect(host.querySelector('[data-action=copy-raw]')).toBe(button);
  button.click(); await vi.waitFor(() => expect(writeText).toHaveBeenCalledOnce());
  expect(JSON.parse(writeText.mock.calls[0][0])).toEqual({type:'aircraft',aircraft:[{icao:'ABC123',callsign:'<NEW & 飞行>'}]});
  expect(button.textContent).toBe('Copied');
  expect(host.querySelector('details')!.open).toBe(false);
});

it('falls back on HTTP or denied clipboard access and restores focus', async () => {
  vi.stubGlobal('navigator', { clipboard: { writeText: vi.fn().mockRejectedValue(new Error('denied')) } });
  const button = document.createElement('button'); document.body.append(button); button.focus();
  const exec = vi.fn(() => { expect((document.activeElement as HTMLTextAreaElement).value).toBe('{"exact":"原文"}'); return true; });
  Object.defineProperty(document,'execCommand',{configurable:true,value:exec});
  await copyDetails(button,'{"exact":"原文"}');
  expect(exec).toHaveBeenCalledWith('copy'); expect(document.activeElement).toBe(button);
  expect(document.querySelector('textarea')).toBeNull();
  expect(button.textContent).toBe('Copied');
  delete (document as Partial<Document>).execCommand;
});
