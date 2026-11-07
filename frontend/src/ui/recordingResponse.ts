/** Recording endpoints publish an exact length; enforce it before allocation and on every chunk. */
export async function readRecordingResponse(response: Response, minimum: number, maximum: number): Promise<ArrayBuffer> {
  const reader = response.body?.getReader();
  try {
    const header = response.headers.get('Content-Length'), length = Number(header);
    if (!header || !/^\d+$/.test(header) || !Number.isSafeInteger(length) || length < minimum || length > maximum || !reader)
      throw new Error('Invalid recording response length');
    const data = new Uint8Array(length); let at = 0;
    for (;;) {
      const { done, value } = await reader.read(); if (done) break;
      if (value.byteLength > data.length - at) throw new Error('Recording response exceeded declared length');
      data.set(value, at); at += value.byteLength;
    }
    if (at !== data.length) throw new Error('Incomplete recording response');
    return data.buffer;
  } finally {
    if (reader) { await reader.cancel().catch(() => {}); reader.releaseLock(); }
  }
}
