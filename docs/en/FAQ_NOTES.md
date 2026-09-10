# HAROGIC FAQ and Operations

## SDK Call Constraints

1. Only one SDK call may access a device at a time; WebSA serializes calls with command and device locks.
2. A device cannot be shared by the API and SAStudio4; `Device_Open` fails while another application owns it.
3. Configuration calls may be repeated. Fields not explicitly set inherit SDK Profile defaults.
4. Continuous acquisition requires repeated Get calls. WebSA does not continuously fetch spectra when no browser is connected.

## SWP and RTA

- `SWP_GetFullSweep` can return a range wider than requested; WebSA trims it with `DSP_InterceptSpectrum`.
- Requested and device-effective point counts can differ. STATUS exposes request and actual values separately.
- RBW depends on sample rate, window, decimation, and FFT size. RTA Auto RBW must use the SDK actual value rather than a simple `span/2000` calculation.
- SWP and RTA independently retain Center, Span, Ref, RBW, VBW, Sweep, and actual values. See [MODE_STATE_FLOW.md](MODE_STATE_FLOW.md).
- After eight consecutive RTA Trigger/Get failures, WebSA reconfigures RTA in place up to two times; persistent failure causes the supervisor to restart the worker.
- Use `rta_health.error_streak/recovery_attempts` to diagnose a stalled RTA stream.
- While a harmonic/PNM measurement is active, SWP-owned commands (frequency, Ref, RBW, VBW, sweep, points, spur, window, gain, reference clock) are rejected with an explicit error so the session cannot be disturbed; in RTA, FFT window/points/spur are also SWP-only.

## Reference Level

- Manual Ref configures the active SWP/RTA Profile; it is not only a display-axis adjustment.
- Auto Ref only adjusts when the peak is at least 15 dB above the estimated noise floor and the "about 5 dB above peak" target is not below -50 dBm; a high noise floor also keeps about 30 dB of headroom. With no signal or a too-weak peak it holds current Ref instead of converging to -50 dBm.
- When Auto is active, a Center change or an SWP/RTA return temporarily raises Ref to 0 dBm if it was below zero, avoiding retuning to an unknown strong signal with an unsafe low Ref.
- Every SWP/RTA reconfiguration clears stale candidates and waits 0.75 seconds before Auto Ref observations resume.
- Auto Ref remains selected but is suspended under manual Atten; it resumes when Atten returns to Auto.
- Lower Ref and RBW generally reduce the displayed noise floor, but input overload must be avoided.

## Reference Clock

- `ReferenceClockSource`: 0=Int, 1=Ext (automatic fallback when unlocked), 2=Int+ DOCXO, 3=ExtForce.
- The SAN-90 external reference input is 10 MHz; WebSA sets `ExternalSystemClockFrequency=10 MHz`.
- `EnableReferenceClockOut` controls reference output; support depends on the hardware model.
- Changing Ref Clock or Clock Output in RTA reconfigures the RTA Profile without applying SWP configuration or leaving RTA.
- `SystemClockSource=External` is dangerous and must only be used under vendor guidance; WebSA does not expose it.
- GNSS 1PPS calibration requires a real, locked 1PPS input. The vendor DLL can block when no signal is present.
- Do not use `external_forced` without a valid external reference.

## Phase Noise

- Results near the 100 Hz and 10 MHz offset boundaries can be inaccurate; an SWP span of twice the maximum offset can be used as a cross-check.
- Typical minimum input power is -50 dBm.
- PNM is incremental: each Get returns a partially updated result.

## Configuration Environment Variables

| Variable | Default | Description |
|---|---|---|
| `WEBSA_HOST` | `127.0.0.1` | HTTP/WS listen address |
| `WEBSA_PORT` | `8080` | Service port |
| `WEBSA_TOKEN` | empty | REST Bearer token and browser/WS query token |
| `WEBSA_ALLOWED_ORIGINS` | empty | Additional allowed Origins, comma-separated |
| `WEBSA_ALLOW_UNAUTHENTICATED_REMOTE` | empty | Explicitly allow remote access without a token; trusted isolated networks only |
| `WEBSA_LOG` | `INFO` | Python log level |
| `WEBSA_LOGFILE` | empty | Optional rotating log file (5 MiB, 3 backups) |
| `WEBSA_STATIC` | automatic | Override frontend directory |
| `HTRA_API_LIB` | derived from `/opt/htraapi` and host architecture | Full `libhtraapi.so` path override |

Remote listeners require a token:

```bash
WEBSA_HOST=0.0.0.0 WEBSA_TOKEN='replace-with-a-long-random-token' ./run.sh
```

Open the browser at:

```text
http://device-address:8080/?token=the-same-token
```

When a reverse proxy changes the Origin:

```bash
WEBSA_ALLOWED_ORIGINS='https://sa.example.com' \
WEBSA_HOST=127.0.0.1 WEBSA_TOKEN='...' ./run.sh
```

Query tokens can enter browser history and proxy logs, so remote access should use HTTPS. Set `WEBSA_ALLOW_UNAUTHENTICATED_REMOTE=1` only on a trusted isolated network.

## Startup and Recovery

Always run the project scripts from the repository root:

```bash
./build.sh
./run.sh
./stop.sh
```

`run.sh` starts a supervisor and a WebSA worker. The supervisor restarts the worker with backoff after a native crash, fatal acquisition error, or DLL timeout; configuration errors are not restarted in a loop. A simulated worker `SIGKILL` restored the SAN-90 API in about three seconds. The default runtime log is `/tmp/san90-web.log`.

If the device still does not recover:

1. Stop SAStudio4 and any other API process.
2. Wait a few seconds, then run `./stop.sh && ./run.sh`.
3. Replug USB if it still fails; a fully wedged firmware can require a system restart.

## Software Verification

Run the complete gate with:

```bash
./test.sh
python3 -m ruff check web_sa tests tools
cd frontend/modern && npm audit
```

`./test.sh` runs backend pytest, Ruff, and frontend Vitest, and returns a non-zero status if any stage fails. The current baseline is 51 backend tests and 23 frontend tests.

## SAN-90 / TinySA Hardware Smoke Test

With the service running:

```bash
./tools/hardware_smoke.py --tinysa-port /dev/ttyACM0
```

This command reads TinySA identity without changing TinySA output, but exercises SAN SWP/RTA and restores the SAN SWP Center/Span that existed before the test.

To explicitly transmit a 1 GHz TinySA signal:

```bash
./tools/hardware_smoke.py \
  --tinysa-port /dev/ttyACM0 \
  --configure-tinysa \
  --tinysa-output-mode normal \
  --frequency 1e9 --span 10e6 --duration 3
```

Safety limits:

- `<=6 GHz`: `-25 dBm`
- `(6,7] GHz`: `-35 dBm`
- `(7,9] GHz`: `-42 dBm`
- `>9 GHz`: rejected

The tool first disables output and sets -42 dBm, selects the output mode, sets and reads back frequency with `sweep cw`, then applies the band-safe level and runs `output on + resume`. It always executes `output off` on completion or failure. It does not restore the previous TinySA frequency/mode; record shared-bench settings before use and restore them afterward when needed.

## Other Known Behavior

- IF Gain grades 1/4 can differ by about 1 dB at some frequencies.
- SAN-45/60/90 nominal ranges are 9 kHz-4.5/6/9 GHz. Features are shared, but specifications differ.
- Some firmware leaves `RefClkFreqOffset` at zero; use the calculated value after calibration when available.
- RTA currently uses the first spectrum in PacketFrame. Hardware bitmap/PacketFrame density semantics still need comparison against the vendor application.
