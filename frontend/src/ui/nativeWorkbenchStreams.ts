import { decodeIngressClient, type DecodeIngressClient } from '../sdr/decodeIngressPool';
import { receiverHasAudio } from '../core/multichannel';
import { onCommandReply, onCommandSent } from '../core/wsSend';
import { claimNativeAudio, forgetNativeAudio, getNativeAudioStatus, sampleNativeAudioWaveform, setNativeAudioRoute, setNativeMaster } from '../audio/sdrAudio';
import { acknowledgeDecoderLog, addDecoderRaster, addDecoderResult, annotateDecoderResult, clearDecoderStatuses, clearGpsSnapshots, decoderLog, endDecoderRasters, expireNativeMeasurements, forgetDecoderLog } from '../sdr/decoderLog';
import type { DecodeEnvelope } from '../sdr/decodeProtocol';
import { claimNativeDecode, publishNativeDecode } from '../sdr/decodeOwnership';
import type { WorkbenchSession } from './workbenchSession';

/** One browser consumer per logical source. Views never own workers, sockets or the speaker graph. */
export function mountNativeWorkbenchStreams(session: WorkbenchSession) {
  const release = claimNativeAudio(session.snapshot);
  const releaseDecode = claimNativeDecode();
  const toggleMute = (): void => { session.masterMuted = !session.masterMuted; session.notify(); };
  const playbackStatus = (): void => session.notify();
  const waveformConsumers = new Map<string, string>();
  const playbackOwner = (uid: string): string => session.duplicateMonitor(uid) ?? uid;
  const releaseWaveform = (uid: string): void => {
    const owner = waveformConsumers.get(uid); waveformConsumers.delete(uid);
    if (owner && ![...waveformConsumers.values()].includes(owner)) sampleNativeAudioWaveform(owner, null);
  };
  const waveform = (uid: string, samples: Float32Array<ArrayBuffer> | null): boolean => {
    if (!samples) { releaseWaveform(uid); return false; }
    const owner = playbackOwner(uid);
    if (waveformConsumers.get(uid) !== owner) releaseWaveform(uid);
    if (!sampleNativeAudioWaveform(owner, samples)) { releaseWaveform(uid); return false; }
    waveformConsumers.set(uid, owner); return true;
  };
  document.addEventListener('websa:monitor-toggle-mute', toggleMute);
  document.addEventListener('websa:monitor-status', playbackStatus);
  let pendingMainMonitor: { service: string | undefined; channel: number; frequency: number | null;
    requestId: unknown; command: unknown; acknowledged: boolean; expires: number } | null = null;
  const stopMainReplies = onCommandReply(reply => {
    if (!pendingMainMonitor) return;
    if (reply.cmd === 'DISCONNECTED') { pendingMainMonitor = null; return; }
    if (reply.request_id !== pendingMainMonitor.requestId || reply.response_to !== pendingMainMonitor.command) return;
    if (reply.cmd === 'ERROR') pendingMainMonitor = null;
    else if (reply.cmd === 'STATUS') { pendingMainMonitor.acknowledged = true; session.notify(); }
  });
  // A main-panel restart after Stop all restores that receiver's local route. Keep
  // the single UID speaker owner and the user's gain/pan/master preferences.
  // Playback opens only after a healthy service confirmation; local output gain
  // does not pause the PCM clock or its pre-gain meter.
  const stopMainCommands = onCommandSent(command => {
    if (command.cmd === 'SET_PRESET' || command.cmd === 'DISCONNECT'
      || command.cmd === 'SET_MODE' && command.mode !== 'sdr'
      || command.cmd === 'SET_SDR_CHANNEL' && command.enabled === false
        && Number(command.channel ?? 1) === pendingMainMonitor?.channel) pendingMainMonitor = null;
    const snapshot = session.snapshot;
    const channel = command.cmd === 'SET_SDR' ? 1 : Number(command.channel ?? 1);
    const receiver = snapshot.receivers.find(r => r.display_number === channel);
    const startsReceiver = command.cmd === 'SET_SDR' && typeof command.listen === 'number'
      || command.cmd === 'SET_SDR_CHANNEL' && command.enabled === true
        && (typeof command.freq === 'number' || typeof command.mode === 'string');
    if (!startsReceiver) return;
    const replacesPending = pendingMainMonitor?.channel === channel;
    if (snapshot.receivers.some(r => r.confirmed.enabled) && !replacesPending) return;
    if (!receiver || replacesPending) {
      // Retiring a channel removes its UID. Wait for the new logical receiver
      // created by this explicit tune, then attach its browser speaker route.
      const frequency = command.cmd === 'SET_SDR' ? command.listen : command.freq;
      pendingMainMonitor = { service: snapshot.service_id, channel,
        frequency: typeof frequency === 'number' ? frequency : null, requestId: command.request_id,
        command: command.cmd, acknowledged: false, expires: Date.now() + 8000 };
      return;
    }
    pendingMainMonitor = null;
    session.monitorMode = 'manual';
    session.monitor(receiver.receiver_uid, { enabled: true });
  });
  const decoders = new Map<string, { worker: DecodeIngressClient; service: string | undefined; ended: boolean }>();
  const processingSources = new Map<string, string>();
  const errors = new Map<string, string>();
  const audioKeys = new Map<string, string>();
  let disposed = false;
  let service = session.snapshot.service_id;
  const stopDecoder = (uid: string): void => {
    const owner = decoders.get(uid); if (owner && !owner.ended) owner.worker.terminate();
    decoders.delete(uid); processingSources.delete(uid);
  };
  const sync = (): void => {
    if (disposed) return;
    const snapshot = session.snapshot;
    for (const [uid, owner] of waveformConsumers) if (service !== snapshot.service_id || !session.playbackActive(owner) || playbackOwner(uid) !== owner) releaseWaveform(uid);
    if (pendingMainMonitor) {
      const pending = pendingMainMonitor;
      if (pending.service !== snapshot.service_id || Date.now() >= pending.expires) pendingMainMonitor = null;
      else {
        const receiver = snapshot.receivers.find(r => r.display_number === pending.channel);
        if (pending.acknowledged && receiver?.state === 'running' && !receiver.fault && receiverHasAudio(snapshot, receiver.confirmed)
          && (pending.frequency === null || Math.abs(receiver.confirmed.baseband.frequency_hz - pending.frequency) < 0.5)) {
          pendingMainMonitor = null;
          if (!session.routes.has(receiver.receiver_uid)) {
            session.monitorMode = 'manual';
            session.routes.set(receiver.receiver_uid, { ...session.route(receiver.receiver_uid), enabled: true });
          }
        }
      }
    }
    // A UID is scoped to its service. Retire the owner before dropping history,
    // so already queued callbacks cannot recreate a released result cache.
    for (const uid of audioKeys.keys()) if (service !== snapshot.service_id || !snapshot.receivers.some(r => r.receiver_uid === uid)) {
      stopDecoder(uid); forgetNativeAudio(uid); audioKeys.delete(uid); forgetDecoderLog(uid); errors.delete(uid);
    }
    service = snapshot.service_id;
    for (const receiver of snapshot.receivers) if (receiver.state !== 'running' || receiver.processing_state === 'failed' || !receiver.confirmed.processors.length || receiver.fault) endDecoderRasters(receiver.receiver_uid);
    setNativeMaster(session.masterGain, session.masterMuted);
    for (const [uid, decoder] of decoders) if (decoder.service !== snapshot.service_id) stopDecoder(uid);
    for (const receiver of snapshot.receivers) {
      const uid = receiver.receiver_uid, config = receiver.confirmed;
      const key = `${snapshot.service_id}:${receiver.stream?.epoch}:${receiver.stream?.channel_id}:${receiver.processing_runtime?.processing_generation}:${config.processors[0]?.mode}`;
      const restart = audioKeys.has(uid) && audioKeys.get(uid) !== key;
      if (restart) {
        errors.delete(uid); endDecoderRasters(uid);
        // A disabled receiver loses its stream and removing a processor loses
        // its mode before the old subscription's accepted tail is delivered.
        if (receiver.state === 'running' && config.processors.length) stopDecoder(uid);
      }
      setNativeAudioRoute(uid, config.processors[0]?.mode ?? '', session.playbackActive(uid), session.route(uid).gain, session.route(uid).pan, restart);
      audioKeys.set(uid, key);
      if (receiver.processing_state === 'failed' || receiver.reception && receiver.reception.state!=='receiving') {clearDecoderStatuses(uid);stopDecoder(uid);continue;}
      if (receiver.state !== 'running' || !config.processors.length || receiver.fault) {
        // STATUS can precede the socket's accepted FIFO tail. Only an ended
        // owner is ready to release on this observation.
        if (decoders.get(uid)?.ended) stopDecoder(uid);
        continue;
      }
      if (decoders.has(uid) || errors.has(uid)) continue;
      const worker = decodeIngressClient();
      const owner = { worker, service: snapshot.service_id, ended: false }; decoders.set(uid, owner);
      const fail = (message: string): void => { if (decoders.get(uid) !== owner || owner.ended) return; clearDecoderStatuses(uid); endDecoderRasters(uid); errors.set(uid, message); stopDecoder(uid); session.notify(); };
      worker.onmessage = event => {
        if (decoders.get(uid) !== owner || owner.ended || session.snapshot.service_id !== owner.service) return;
        const message = event.data;
        if (message.type === 'frames') {
          try {
            for (const envelope of message.envelopes as DecodeEnvelope[]) {
              if (envelope.receiver_uid !== uid || envelope.service_id !== owner.service || envelope.channel !== receiver.display_number) throw new Error('Decoded output logical source mismatch');
              processingSources.set(uid, `${envelope.epoch}:${envelope.channel_id}`);
              const previous = decoderLog(uid).entries[0];
              consumeReceiverResult(uid, envelope);
              if (envelope.event.type !== 'start' && decoderLog(uid).entries[0] !== previous) annotateDecoderResult(uid, { input_index: envelope.input_index, input_rate: envelope.input_rate,
                fraction_num: envelope.input_fraction_num, fraction_den: envelope.input_fraction_den, delay_seconds: envelope.delay_seconds,
                delay_known: envelope.delay_known, causal_presentation: envelope.causal_presentation, baseband_stream: envelope.baseband_stream });
            }
            // Decoding updates the retained log immediately. Views can coalesce paint
            // without re-running every receiver's controls and speaker routing.
            acknowledgeDecoderLog(uid); worker.postMessage({ type: 'ack' });
            publishNativeDecode(message.envelopes as DecodeEnvelope[]); session.notifyResults(uid);
            // A fresh validated delivery confirms this subscription recovered.
            if (errors.delete(uid)) session.notify();
          } catch (error) { fail(String(error)); }
        } else if (message.type === 'ended') {
          // Hold this terminated owner while a stale directory still says it is
          // running; a confirmed stop, new stream or explicit retry releases it.
          owner.ended = true; worker.terminate(); processingSources.delete(uid);
          clearDecoderStatuses(uid); clearGpsSnapshots(uid); endDecoderRasters(uid); errors.delete(uid);
          session.notifyResults(uid); session.notify();
        } else if (message.type === 'error') fail(message.message);
        else if (message.type === 'closed') {
          endDecoderRasters(uid);
          const receiver = session.snapshot.receivers.find(r => r.receiver_uid === uid);
          if (!receiver || receiver.state !== 'running' || !receiver.confirmed.processors.length || receiver.fault) {
            stopDecoder(uid); errors.delete(uid); if (!receiver) forgetDecoderLog(uid);
          } else errors.set(uid, message.message);
          session.notify();
        }
        // A socket opening does not yet prove that decoding has resumed.
        else if (message.type === 'connected') { session.notify(); }
      };
      worker.onerror = event => fail(event.message || 'Decoder worker failed');
      const url = new URL('/ws/decode', location.href); url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
      url.searchParams.set('receiver_uid', uid); worker.postMessage({ type: 'start', url: url.href });
    }
  };
  const expiry = setInterval(() => { for (const uid of expireNativeMeasurements()) session.notifyResults(uid); }, 500);
  const unsubscribe = session.subscribe(sync); sync();
  return {
    results: (uid: string) => ({ ...(session.snapshot.receivers.some(r => r.receiver_uid === uid) ? decoderLog(uid) : { entries: [], evicted: 0 }), source: processingSources.get(uid) ?? null }),
    playback: (uid: string) => getNativeAudioStatus(playbackOwner(uid)),
    waveform,
    status: (uid: string): string => errors.get(uid) ?? String(getNativeAudioStatus(playbackOwner(uid))?.error ?? ''),
    retry(uid: string): void { errors.delete(uid); stopDecoder(uid); audioKeys.delete(uid); forgetNativeAudio(uid); sync(); session.notify(); },
    hasAudio: (uid: string) => { const r = session.snapshot.receivers.find(r => r.receiver_uid === uid); return !!r && receiverHasAudio(session.snapshot, r.confirmed); },
    dispose(): void { if (disposed) return; disposed = true; clearInterval(expiry); stopMainCommands(); stopMainReplies(); document.removeEventListener('websa:monitor-toggle-mute', toggleMute); document.removeEventListener('websa:monitor-status', playbackStatus); unsubscribe(); for (const uid of waveformConsumers.keys()) releaseWaveform(uid); for (const uid of decoders.keys()) stopDecoder(uid); for (const uid of audioKeys.keys()) forgetDecoderLog(uid); releaseDecode(); release(); },
  };
}

/** Normalize native events into the shared bounded result renderer without using selected-slot globals. */
export function consumeReceiverResult(uid: string, envelope: DecodeEnvelope): void {
  const event = envelope.event, source = `${envelope.epoch}:${envelope.channel_id}`;
  const result = (kind: 'text' | 'table' | 'status', data: unknown): void => addDecoderResult({ provider: 'native', mode: envelope.mode, kind, data }, source, uid);
  if (event.type === 'start') { clearDecoderStatuses(uid); clearGpsSnapshots(uid); endDecoderRasters(uid); return; }
  if (event.type === 'processing-end') {
    clearDecoderStatuses(uid); clearGpsSnapshots(uid); endDecoderRasters(uid); return;
  }
  if (event.type === 'decoder-result') {
    const rttyState = ['rtty', 'rtty-reverse'].includes(envelope.mode) && event.kind === 'table'
      && event.data && typeof event.data === 'object' && !Array.isArray(event.data) && 'state' in event.data;
    addDecoderResult({ provider: event.provider!, mode: envelope.mode, kind: rttyState ? 'status' : event.kind!,
      data: rttyState ? { ...event.data as object, type: 'rtty-state' } : event.data }, source, uid); return;
  }
  if (event.type === 'sstv-image' || event.type === 'radiofax-image') {
    const { width: w, row, rows } = event;
    if (!Number.isInteger(w) || !Number.isInteger(row) || !Number.isInteger(rows)) throw new Error('Invalid image geometry');
    const height = event.type === 'sstv-image' ? event.height : event.meta?.lines;
    let rgb: Uint8Array;
    if (event.type === 'sstv-image') {
      if (Array.isArray(event.rgb) && event.rgb.some(value => !Number.isInteger(value) || value < 0 || value > 255)) throw new Error('Invalid native RGB pixels');
      rgb = typeof event.rgb === 'string' ? Uint8Array.from(atob(event.rgb), c => c.charCodeAt(0)) : Uint8Array.from(event.rgb ?? []);
    }
    else {
      const gray = Uint8Array.from(atob(event.gray ?? ''), c => c.charCodeAt(0)); rgb = new Uint8Array(gray.length * 3);
      gray.forEach((value, i) => rgb.fill(value, i * 3, i * 3 + 3));
    }
    addDecoderRaster(uid, envelope.mode, source, { id: `${event.meta?.transmissionId ?? ''}:${event.meta?.serial ?? ''}:${event.meta?.segmentIndex ?? 0}`,
      label: event.meta?.name ?? envelope.mode, w: w!, h: height!, row: row!, rows: rows!, rgb,
      final: event.meta?.complete === true, partial: event.meta?.partial === true, missingRows: event.meta?.missingRows, grow: event.type === 'radiofax-image' }); return;
  }
  if (event.type === 'ft8') result('table', { decodes: [{ snr_db: event.snrDb, dt_s: event.timeOffsetS, audio_hz: event.frequencyHz,
    msg: event.text, center_hz: event.centerHz, window_start: event.windowStart ?? envelope.input_index }] });
  else if (event.type === 'rf-measurement') result('status', event);
  else if (event.type === 'cw') {
    result('status', { ...event, type: 'cw-level', text: undefined });
    annotateDecoderResult(uid, { input_index: envelope.input_index, input_rate: envelope.input_rate, fraction_num: envelope.input_fraction_num,
      fraction_den: envelope.input_fraction_den, delay_seconds: envelope.delay_seconds, delay_known: envelope.delay_known, causal_presentation: envelope.causal_presentation });
    if (event.text || event.endsLine) result('text', { text: (event.text ?? '') + (event.endsLine ? '\n' : ''), endsLine: event.endsLine === true, share: event.share });
  }
  else if (event.type === 'drm' || event.type === 'drm-constellation') {
    // Both messages describe one live receiver. Keep the plot and heartbeat metadata
    // together, scoped to this UID and stream, instead of discarding the constellation.
    const previous = decoderLog(uid).entries.find(entry => entry.source === source && entry.mode === envelope.mode
      && entry.kind === 'status' && (entry.data as { type?: string })?.type === 'drm')?.data as Record<string, unknown> | undefined;
    const lines = Array.isArray(event.lines) && event.lines.length ? event.lines : previous?.lines ?? [];
    result('status', { ...previous, ...event, type: 'drm', lines,
      snrDb: event.snrDb === null ? null : Number.isFinite(event.snrDb) ? event.snrDb : Array.isArray(event.lines) && event.lines.length ? null : previous?.snrDb,
      points: event.status?.locked === false ? [] : event.points ?? previous?.points ?? [], input_index: envelope.input_index });
  }
  else if (event.type === 'wfm-info' || event.type === 'am-info') result('status', event);
  else result('status', event);
}
