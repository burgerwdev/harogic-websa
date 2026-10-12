// Device detail popover — mirrors the GNSS popover (same class, same open/close flow).
// The rows are filled live by the STATUS handler in core/ws.ts (dev-d-* ids), so opening
// the popover only has to show it.
export function fillDeviceDetail() {
  const pop = document.getElementById('device-popover');
  if (pop) pop.style.display = '';
}

export function closeDeviceDetail() {
  const pop = document.getElementById('device-popover');
  if (pop) pop.style.display = 'none';
}
