# API Documentation

Backend (`web_sa/`) external interfaces: **HTTP REST** + **WebSocket** (JSON commands/status + binary trace frames).

- Service: `http://127.0.0.1:8080` (WebSocket at `/ws`)
- Frontend: modern UI at `/`, static assets `/static/modern/dist/{file}`
- Loopback is the secure default. Remote listeners require `WEBSA_TOKEN`; REST accepts a
  Bearer token and browser/WS clients accept `?token=...`. WebSockets require same-origin
  access or an origin listed in `WEBSA_ALLOWED_ORIGINS`.

---

## 1. HTTP Endpoints

### `GET /api/state`

Returns the full device state (JSON, same shape as WS `STATUS`).

```bash
curl http://localhost:8080/api/state
```

Example response:

```json
{
  "cmd": "STATUS",
  "connected": true,
  "device": "SAN-90",
  "device_detail": { "uid": "5230500C00380025", "model": 67, "hw": 8, "mfw": 14184, "ffw": 14184, "bus_speed": 12, "bus_ver": 1, "api_ver": "0.55.89", "warnings": "" },
  "center": 1000000000.0, "span": 600000000.0, "ref": 0.0,
  "rbw_mode": "auto", "rbw": 100000.0, "vbw_mode": "bypass", "vbw": 1000000.0,
  "points": 1000, "window": 1, "spur": "standard", "mode": "std", "pnm_supported": true,
  "caps": { "model": 67, "name": "SAN-90", "fmin": 9000, "fmax": 9000000000 },
  "preset_defaults": { "center": 1000000000, "span": 100000000, "rbw": 100000, "points": 1000, "ref": 0, "atten": -1 },
  "req": { "center": 1000000000, "span": 600000000, "points": 1000, "rbw_mode": "auto", "rbw": 100000, "vbw_mode": "bypass", "vbw": 1000000, "ref": 0, "spur": "standard" },
  "actual": { "center": 1000000000, "span": 600000000, "points": 985, "rbw": 100000, "vbw": 1000000 },
  "amp": { "atten": -1, "preamp": 0, "ifgain": 2, "gain_strategy": 0, "atten_actual": 30, "preamp_actual": 1, "ifgain_actual": 2 },
  "ref_clock": "internal", "has_docxo": false, "refclk_ppm": 0.0, "calibrating": false,
  "refclk_out": false, "last_cal_freq": 0.0, "gnss": { "lock": false, "sats": 0, "docxo": 0, "docxo_mode": -1, "antenna": -1, "latitude": 0.0, "longitude": 0.0, "altitude": 0, "time": "0000-00-00 00:00:00" },
  "last_error": ""
}
```

Field reference:

| Field | Type | Description |
|---|---|---|
| `connected` | bool | device connected |
| `device` / `device_detail` | str / obj | device name + details (uid/model/hw/mfw/ffw/bus/api/warnings) |
| `center` / `span` / `ref` | number | effective values for the active hardware mode |
| `ref_mode` | str | active reference-level mode (manual/auto) |
| `rbw_mode` / `rbw` | str / number | RBW mode (manual/auto), resolution bandwidth |
| `vbw_mode` / `vbw` | str / number | VBW mode (bypass/equal/tenth/manual), video bandwidth |
| `points` | int | requested points (frontend resample target) |
| `window` | int | FFT window: 0=FlatTop 1=Blackman-Nuttall 2=LowSideLobe 3=Rectangle 4=Kaiser |
| `spur` | str | spur rejection (bypass/standard/enhanced) |
| `mode` | str | measurement mode (std/harmonic/pnm/rta) |
| `caps` | obj | model capabilities (model/name/fmin/fmax) |
| `preset_defaults` | obj | device default config (used by Preset) |
| `req` / `actual` | obj | active request/actual values; `req.swp` and `req.rta` retain mode-private settings |
| `swp_actual` / `rta_actual` | obj | latest SDK effective settings for each spectrum mode |
| `config_version` / `response_to` | int / str? | successful reconfiguration sequence and command-response correlation |
| `amp` | obj | gain chain: atten/preamp/ifgain/gain_strategy + actual atten_actual/preamp_actual/ifgain_actual |
| `ref_clock` | str | reference clock source: internal/external/premium/external_forced |
| `has_docxo` | bool | DOCXO supported |
| `refclk_ppm` / `calibrating` / `last_cal_freq` | - | GNSS calibration state |
| `refclk_out` | bool | reference clock output enable |
| `gnss` | obj | GNSS state: `lock`(0/1) `sats` `docxo`(0/1) `docxo_mode`(0=disciplined,1=hold) `antenna`(0=external,1=internal) `latitude`/`longitude`(deg) `altitude`(m) `time`(UTC, "0000-00-00 00:00:00" when invalid) |
| `last_error` | str | recent error message |

### `POST /api/config`

Send a command via REST (equivalent to WS commands, see table below). Response is the full STATUS.

```bash
curl -X POST http://localhost:8080/api/config \
  -H 'Content-Type: application/json' \
  -d '{"cmd": "SET_FREQ", "center": 1090000000, "span": 600000000}'
```

### `GET /`

Returns the frontend page (modern UI `dist/index.html`).

### `GET /static/modern/dist/{file}`

Frontend static assets (JS/CSS/fonts).

---

## 2. WebSocket Protocol

### Connect

```
ws://localhost:8080/ws
```

After connecting, the server starts pushing trace frames (FREQ/POWR) and status.

### 2.1 Client → Server (commands)

JSON object: `{"cmd": "<COMMAND>", ...}`

| Command | Params | Description |
|---|---|---|
| `STATUS` | - | request one full status (STATUS reply) |
| `CONNECT` | - | connect device (if not connected) |
| `SET_PRESET` | - | restore device default config (Preset) |
| `CAL_REFCLK` | `count?` | GNSS 1PPS reference clock calibration (background; `calibrating=true` meanwhile) |
| `SET_FREQ` | `center`,`span` or `start`,`stop` | atomically set the SWP frequency window |
| `SET_REF` | `mode` (manual/auto), `ref?` | active-mode reference level; manual requires ref |
| `SET_RBW` | `mode?` (manual/auto), `rbw?` | set resolution bandwidth |
| `SET_VBW` | `mode?` (manual/equal/tenth/bypass), `vbw?` | set video bandwidth |
| `SET_POINTS` | `points` (51~4000) | set sweep points |
| `SET_SPUR` | `mode` (bypass/standard/enhanced) | spur rejection mode |
| `SET_WINDOW` | `window` (0~4) | FFT window |
| `SET_AMP` | `atten` (-1~33), `preamp` (0/1), `ifgain` (0~3), `gain_strategy` (0/1) | gain chain config |
| `SET_REFCK` | `mode` (internal/external/premium/external_forced) | reference clock source; reconfigures the active RTA profile |
| `SET_REFCKOUT` | `on` (bool) | reference clock output; reconfigures the active RTA profile |
| `SET_MODE` | `mode` (std/harmonic/pnm/rta) | switch measurement mode (session) |
| `SET_RTA` | `center?`, `span?` | atomically set RTA center and 2^n analysis span |
| `SET_HARM` | `f0`, `count`, `span` | harmonic params (fundamental Hz, orders, span per harmonic) |
| `SET_PNM` | `center`, `threshold`, `traceavg`, `start`, `stop` | phase noise params |

> Config commands (SET_*) automatically reply with the latest STATUS.

**Command examples:**

```js
// set frequency and switch to harmonic measurement
ws.send(JSON.stringify({ cmd: 'SET_FREQ', center: 1e9, span: 100e6 }));
ws.send(JSON.stringify({ cmd: 'SET_MODE', mode: 'harmonic' }));
ws.send(JSON.stringify({ cmd: 'SET_HARM', f0: 1e9, count: 5, span: 1e6 }));
```

### 2.2 Server → Client

#### JSON messages

| `cmd` | Trigger | Description |
|---|---|---|
| `STATUS` | on connect / after commands / **periodic (every 1 s)** | full status (fields in §1); periodic push keeps GNSS/time/lock states fresh without page refresh |
| `HARM` | during harmonic measurement | result: `{cmd:'HARM', list:[{n,f,amp,dBc,idx,inSpan}...]}` |
| `PNM` | during phase noise | result: `{cmd:'PNM', offset[], pn[], carrier_freq, carrier_power, progress, done}` |
| `ERROR` | device error | `{cmd:'ERROR', msg}` |

**PNM message fields:**

```json
{
  "cmd": "PNM",
  "carrier_freq": 999999900.0,   // carrier frequency (Hz)
  "carrier_power": -20.1,        // carrier power (dBm)
  "offset": [100, 1000, 10000, 100000, 1000000, 10000000],   // offset points (Hz)
  "pn": [-82.8, -95.5, -104.3, -117.1, -126.4, -132.0],      // phase noise (dBc/Hz)
  "progress": 25,                // incremental acquisition progress 0~100 (refreshes after first round)
  "done": false                  // first-round complete flag
}
```

> Unfilled segments of `pn` are -500 (invalid); the plot is complete when `offset` covers the full range and `pn` has no invalid points.

#### Binary trace frames

**16-byte header + data:**

```
offset  size  type    content
0       4     bytes   magic: 'FREQ' | 'POWR'
4       4     u32     version (FREQ/POWR pairing sequence)
8       4     u32     points
12      4     f32     sweep_ms (measured sweep time)
16      ...           data
```

| magic | data type | description |
|---|---|---|
| `FREQ` | float64 × points | frequency axis (Hz), sent only on version change |
| `POWR` | float32 × points | power trace (dBm), paired with same-version FREQ |

> **Note**: POWR is forced to float32 (prevents interpolation precision misalignment); points is device-native (frontend does peak-preserving resampling).

---

## 3. Measurement Modes

### Harmonic

1. `SET_MODE {mode:'harmonic'}` → `SET_HARM {f0, count, span}`
2. Server auto-tunes per harmonic (H1~H5…), pushes `HARM` per result

### Phase Noise

1. `SET_MODE {mode:'pnm'}` → `SET_PNM {center, threshold, traceavg, start, stop}`
2. Server does incremental acquisition (`PNM_GetPartialUpdatedFullTrace`), pushes `PNM` per frame (with progress)

### Exit measurement

`SET_MODE {mode:'std'}` → restore standard sweep.

---

## 4. Python Example

```python
import asyncio, json, struct
import aiohttp

async def main():
    async with aiohttp.ClientSession() as s:
        # 1) REST state
        async with s.get('http://localhost:8080/api/state') as r:
            st = await r.json()
            print('connected:', st['connected'], '| mode:', st['mode'])

        # 2) WebSocket trace frames
        async with s.ws_connect('ws://localhost:8080/ws') as ws:
            await ws.send_str(json.dumps({'cmd': 'SET_FREQ', 'center': 1e9, 'span': 100e6}))
            async for msg in ws:
                if msg.type == aiohttp.WSMsgType.BINARY:
                    head = struct.unpack('<4sIIf', msg.data[:16])
                    magic, ver, pts, sweep_ms = head[0], head[1], head[2], head[3]
                    if magic == b'FREQ':
                        freq = struct.unpack_from('<%dd' % pts, msg.data, 16)
                    elif magic == b'POWR':
                        import array
                        pwr = array.array('f')
                        pwr.frombytes(msg.data[16:])
                        print('POWR', ver, 'points', pts, 'peak', max(pwr))
                elif msg.type == aiohttp.WSMsgType.TEXT:
                    obj = json.loads(msg.data)
                    print('JSON:', obj.get('cmd'))

asyncio.run(main())
```

---

## 5. Notes

- **Single handle**: only one process may open the device at a time (SAStudio4 and the Web service are mutually exclusive)
- **Serialized device calls**: single-process aiohttp, commands handled serially via `_dispatch`
- **Binary SDK**: `libhtraapi.so` is proprietary, obtain it from HAROGIC (the `htra_api.py` wrapper is bundled)
- **SystemClockSource=External**: dangerous, do not use (hangs the device)
