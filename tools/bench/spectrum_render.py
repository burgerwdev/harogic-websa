#!/usr/bin/env python3
"""GPU pixel parity, context recovery and synthetic spectrum throughput; no radio connection.

Run: python tools/bench/spectrum_render.py --vulkan --frames 90
Use --url for an existing Vite server, --cpu for the Canvas2D fallback.
"""
from __future__ import annotations

import argparse
import json
import os
import signal
import socket
import subprocess
import time
from contextlib import contextmanager
from pathlib import Path
from tempfile import TemporaryDirectory
from urllib.error import URLError
from urllib.request import urlopen

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]

BOOT = r"""async ({points, zoom, mode}) => {
  const root='/static/dist/src/';
  const S=await import(root+'core/store.ts');
  const D=await import(root+'dsp/rtaDensity.ts');
  const R=await import(root+'render/spectrum.ts');
  const redraw=await import(root+'render/redraw.ts');
  const WF=await import(root+'render/waterfall.ts');
  const settings=await import(root+'ui/waterfallState.ts');
  const viewport=await import(root+'ui/spectrumViewport.ts');
  const ref=await import(root+'ui/displayRef.ts');
  const state=await import(root+'ui/sdrState.ts');
  const panels=await import(root+'ui/panels/waterfall.ts');
  const lo=22804687.5, hi=23195312.5;
  state.sdrCenterHz.set(23e6); state.sdrDecimate.set(128);
  S.setSdrMode(mode==='sdr'); S.setRtaMode(mode!=='swp'); S.setViewMode(mode==='swp'?'std':'rta');
  ref.setDisplayRef('preset',-20); S.setDbPerDiv(10);
  settings.rtaHalflife.set(1); settings.rtaDensLayers.set('both'); settings.rtaDensSource.set('live');
  panels.setWaterfall(true); panels.setWfEqualSplit(true);
  const freq=Float64Array.from({length:points},(_,i)=>lo+(hi-lo)*i/(points-1));
  const spec=new Float32Array(points);
  viewport.setCapture({lo,hi});
  if(zoom) { viewport.zoomEnabled.set(true); viewport.setView({lo:23e6-1500,hi:23e6+1500},(hi-lo)/(points-1)*2); }
  S.setFreqArray(freq);
  const d={ver:0,freq,spec,wfRow:new Uint16Array(0),maxDensity:100,startHz:lo,stopHz:hi};
  S.setRtaData(d); S.setRtaDisplays([spec,null,null,null]);
  let tick=0, grid=null;
  const step=()=>{
    for(let i=0;i<points;i++) spec[i]=-100+5*Math.sin(i*1.817+tick*.4)+35*Math.exp(-(((i/points-.5)*180)**2))+12*Math.sin(i*.02+tick*.1)**8;
    if(mode!=='swp') grid=D.advanceDensity(grid,spec,128,1,0,1,'CLEAR_WRITE',freq,'live',tick*1000/30);
    if(mode==='rta') grid.population=40;
    S.setRtaDensity2d(grid); S.traces[0].powers=spec; WF.pushSpectrumRow(spec,freq,lo,hi,100); tick++;
  };
  for(let i=0;i<90;i++)step();
  await new Promise(r=>setTimeout(r,350)); S.syncCanvasSize(); R.renderAll();
  window.bench={S,D,R,redraw,step};
  return {w:S.canvas.width,h:S.canvas.height,ratio:S.pixelRatio};
}"""

RUN = r"""async (count) => {
 const {R,redraw,step,S}=window.bench;
 const paint=[],ingest=[],frames=[]; let last=performance.now();
 for(let i=0;i<count;i++) {
   await new Promise(r=>requestAnimationFrame(r));
   const a=performance.now(); frames.push(a-last);last=a;
   step();const b=performance.now();redraw.requestRender();const c=performance.now();
   if(i>10){ingest.push(b-a);paint.push(c-b);}
 }
 const stats=a=>{a.sort((x,y)=>x-y);return {mean:a.reduce((s,x)=>s+x,0)/a.length,p50:a[Math.floor(a.length*.5)],p95:a[Math.floor(a.length*.95)]}};
 return {paint:stats(paint),ingest:stats(ingest),frames:stats(frames),status:redraw.renderStatus(),heap:performance.memory?.usedJSHeapSize,
 density:S.canvas.dataset.densityRenderer,trace:S.canvas.dataset.traceRenderer,view:S.canvas.dataset.frequencyView};
}"""

CHECK = r"""async () => {
 const {DensityTexture}=await import('/static/dist/src/render/densityTexture.ts');
 const {TraceTexture}=await import('/static/dist/src/render/traceTexture.ts');
 const D=await import('/static/dist/src/dsp/rtaDensity.ts');
 const gpu=new DensityTexture(), results=[];
 const cols=93, bins=32, rows=180, top=7, height=163;
 const raster=new Float32Array(cols*bins);
 for(let y=0;y<bins;y++)for(let x=0;x<cols;x++)if((x+y)%5)raster[y*cols+x]=40*Math.pow((x*17+y*11)%101/100,8);
 const canvas=()=>{const c=document.createElement('canvas');c.width=cols;c.height=height;return c;};
 const expected=canvas(),actual=canvas(),source=canvas();
 const ec=expected.getContext('2d',{willReadFrequently:true}),ac=actual.getContext('2d',{willReadFrequently:true}),sc=source.getContext('2d');
 for(const mode of ['hard','soft','probability'])for(const light of [false,true])for(const width of [.7,3,9.25])for(const pitch of [.8,4.375]) {
  const palette=D.densityPalette(.85,D.densityLut(light),mode==='probability');
  const footprint=mode==='probability'?Math.max(pitch,width):width;
  const f={raster,columns:cols,bins,rows,top,height,centre:-1.125,pitch,width:footprint,soft:mode==='soft',probability:mode==='probability',palette,bounds:{firstBin:0,endBin:bins},changed:true};
  const image=sc.createImageData(cols,height);
  if(f.probability)D.paintProbabilityDensity(raster,cols,bins,image.data,f.centre,pitch,footprint,palette,top,height);
  else D.paintDensity(raster,cols,bins,image.data,rows,f.centre,pitch,footprint,.85,D.densityLut(light),f.soft,palette,f.bounds,top);
  sc.putImageData(image,0,0);
  ec.fillStyle=ac.fillStyle=light?'#e8eef3':'#06101b';ec.fillRect(0,0,cols,height);ac.fillRect(0,0,cols,height);
  ec.drawImage(source,0,0);
  if(!gpu.draw(f))throw Error('GPU density unavailable');
  ac.drawImage(gpu.canvas,0,0,cols,height,0,0,cols,height);
  const a=ec.getImageData(0,0,cols,height).data,b=ac.getImageData(0,0,cols,height).data;
  let max=0,sum=0,bad=0;for(let i=0;i<a.length;i++){const d=Math.abs(a[i]-b[i]);max=Math.max(max,d);sum+=d;if(d>2)bad++;}
  results.push({mode,light,width,pitch,max,mean:sum/a.length,bad});
 }
 for(const row of [1,20,4]) {
  raster.fill(0);raster.fill(7,row*cols,(row+1)*cols);
  const palette=D.densityPalette(1,D.densityLut(false));
  const f={raster,columns:cols,bins,rows,top:0,height,centre:2,pitch:4,width:5,soft:false,probability:false,palette,bounds:{firstBin:row,endBin:row+1},changed:true};
  const image=sc.createImageData(cols,height);
  D.paintDensity(raster,cols,bins,image.data,rows,2,4,5,1,D.densityLut(false),false,palette,f.bounds);
  sc.putImageData(image,0,0);
  ec.fillStyle=ac.fillStyle='#06101b';ec.fillRect(0,0,cols,height);ac.fillRect(0,0,cols,height);ec.drawImage(source,0,0);
  if(!gpu.draw(f))throw Error('GPU band update failed');
  ac.drawImage(gpu.canvas,0,0,cols,height,0,0,cols,height);
  const a=ec.getImageData(0,0,cols,height).data,b=ac.getImageData(0,0,cols,height).data;
  let max=0,bad=0;for(let i=0;i<a.length;i++){const d=Math.abs(a[i]-b[i]);max=Math.max(max,d);if(d>2)bad++;}
  results.push({mode:'moving-band',row,max,bad});
 }
 const gl=gpu.canvas.getContext('webgl2');
 const error=gl.getError();
 const loss=gl.getExtension('WEBGL_lose_context');loss.loseContext();
 await new Promise(r=>setTimeout(r,50));
 const f={raster,columns:cols,bins,rows,top,height,centre:10,pitch:4,width:2,soft:false,probability:false,palette:D.densityPalette(1,D.densityLut(false)),bounds:{firstBin:0,endBin:bins},changed:false};
 const lost=gpu.draw(f);loss.restoreContext();await new Promise(r=>setTimeout(r,100));const restored=gpu.draw(f);gpu.dispose();
 const trace=new TraceTexture();
 const tc=document.createElement('canvas');tc.width=120;tc.height=100;const ctx=tc.getContext('2d',{willReadFrequently:true});
 if(!trace.begin({x:10,y:10,w:100,h:80},1))throw Error('GPU trace unavailable');
 trace.moveTo(20,80);trace.lineTo(40,20);trace.lineTo(50,80);
 trace.moveTo(80,80);trace.lineTo(100,30);trace.stroke('#00ff00',1);trace.composite(ctx);
 const pixels=ctx.getImageData(0,0,120,100).data;
 const lit=(x,y)=>pixels[(y*120+x)*4+3]>0;
 let gap=0,peak=0,escaped=0;for(let y=0;y<100;y++)for(let x=0;x<120;x++)if(lit(x,y)){if(x>=55&&x<=75)gap++;if(x>=39&&x<=41&&y<=22)peak++;if(x<10||x>=110||y<10||y>=90)escaped++;}
 trace.dispose();
 return {results,error,lost,restored,trace:{gap,peak,escaped}};
}"""


@contextmanager
def dev_server(url):
    if url:
        yield url
        return
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    with TemporaryDirectory(prefix='websa-render-') as temporary, (Path(temporary) / 'vite.log').open('w+') as log:
        process = subprocess.Popen(
            ['node', 'node_modules/vite/bin/vite.js', '--host', '127.0.0.1',
             '--port', str(port), '--strictPort', '--mode', 'test'],
            cwd=ROOT / 'frontend', stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        url = f'http://127.0.0.1:{port}/static/dist/'
        try:
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    log.seek(0)
                    raise RuntimeError(log.read())
                try:
                    with urlopen(url, timeout=1):
                        break
                except (URLError, TimeoutError):
                    time.sleep(.1)
            else:
                raise RuntimeError('Vite startup timed out')
            yield url
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url')
    parser.add_argument('--vulkan', action='store_true', help='Use Mesa Vulkan on headless Linux')
    parser.add_argument('--cpu', action='store_true')
    parser.add_argument('--frames', type=int, default=90)
    parser.add_argument('--width', type=int, default=3840)
    parser.add_argument('--height', type=int, default=2050)
    parser.add_argument('--dpr', type=float, default=1)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if args.frames < 12:
        parser.error('--frames must be at least 12')
    flags = ['--use-angle=vulkan', '--enable-features=Vulkan', '--disable-vulkan-surface'] if args.vulkan else []
    report = {'viewport': [args.width, args.height, args.dpr], 'cases': []}
    with dev_server(args.url) as url, sync_playwright() as playwright:
        browser = playwright.chromium.launch(args=flags)
        try:
            report['gpu'] = browser.new_browser_cdp_session().send('SystemInfo.getInfo')['gpu']['devices']
            for mode, points, zoom in [('sdr', 8192, False), ('sdr', 2048, False),
                                       ('sdr', 8192, True), ('rta', 8192, False), ('swp', 8192, False)]:
                page = browser.new_page(viewport={'width': args.width, 'height': args.height}, device_scale_factor=args.dpr)
                errors = []
                page.on('pageerror', lambda error, errors=errors: errors.append(str(error)))
                # All spectrum input is synthetic; never reach the user's running receiver.
                page.route_web_socket('**/*', lambda _socket: None)
                if args.cpu:
                    page.add_init_script("""const original = HTMLCanvasElement.prototype.getContext;
                        HTMLCanvasElement.prototype.getContext = function(type, ...args) {
                            return type === 'webgl2' ? null : original.call(this, type, ...args);
                        };""")
                page.goto(url)
                page.wait_for_function('!!window.websaRenderStatus')
                if not report['cases'] and not args.cpu:
                    check = page.evaluate(CHECK)
                    assert check['error'] == 0 and not check['lost'] and check['restored'], check
                    assert check['trace']['peak'] > 0 and check['trace']['gap'] == check['trace']['escaped'] == 0, check
                    assert all(row['bad'] == 0 for row in check['results']), check
                    report['pixels'] = check
                size = page.evaluate(BOOT, {'mode': mode, 'points': points, 'zoom': zoom})
                result = page.evaluate(RUN, args.frames)
                expected = 'canvas2d' if args.cpu else 'webgl2'
                assert (result.get('trace') or 'canvas2d') == expected, result
                if mode != 'swp':
                    assert (result.get('density') or 'canvas2d') == expected, result
                assert not errors, errors
                row = {'mode': mode, 'points': points, 'zoom': zoom, 'canvas': size, **result}
                report['cases'].append(row)
                print(json.dumps(row), flush=True)
                page.close()
        finally:
            browser.close()
    if args.output:
        args.output.write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
