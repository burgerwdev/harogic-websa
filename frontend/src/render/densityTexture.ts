import type { DensityPalette, DensityRasterBounds } from '../dsp/rtaDensity';

const vertex = `#version 300 es
layout(location = 0) in vec2 position;
void main() { gl_Position = vec4(position, 0.0, 1.0); }`;

const fragment = `#version 300 es
precision highp float;
precision highp int;
uniform highp sampler2D raster;
uniform highp sampler2D palette;
uniform vec4 geometry; // first centre, pitch, footprint, first output row
uniform ivec4 extent; // columns, bins, first occupied bin, end occupied bin
uniform int outputHeight;
uniform int maxIndex;
uniform int mode;
out vec4 colour;

float valueAt(int x, int row) {
  if (row < extent.z || row >= extent.w) return 0.0;
  return texelFetch(raster, ivec2(x, row), 0).r;
}
vec4 colourAt(float value) {
  if (!(value > 0.0)) return vec4(0.0);
  int index = min(maxIndex, int(floatBitsToUint(value) >> 15u));
  return texelFetch(palette, ivec2(index & 255, index >> 8), 0);
}
void main() {
  int x = int(gl_FragCoord.x);
  float y = float(outputHeight) - gl_FragCoord.y + geometry.w;
  float first = geometry.x, pitch = geometry.y, width = geometry.z;
  vec4 best = vec4(0.0);
  if (mode == 2) {
    float centre = (y - first) / pitch;
    float radius = max(0.0, (width / pitch - 1.0) * 0.5);
    float low = centre - radius, high = centre + radius;
    // Division error at an exact empty bin must not become a faint observation.
    if (abs(low - round(low)) < 0.00001) low = round(low);
    if (abs(high - round(high)) < 0.00001) high = round(high);
    int a = int(floor(low)), b = int(floor(high));
    float value = mix(valueAt(x, a), valueAt(x, a + 1), fract(low));
    if (radius > 0.0) {
      value = max(value, mix(valueAt(x, b), valueAt(x, b + 1), fract(high)));
      for (int row = max(extent.z, a + 1); row <= min(b, extent.w - 1); row++)
        value = max(value, valueAt(x, row));
    }
    best = colourAt(value);
    best.a = floor(best.a + 0.5);
  } else {
    float halfWidth = width * 0.5, feather = min(1.0, halfWidth);
    int start = max(extent.z, int(floor((y - 0.5 - halfWidth - first) / pitch)));
    int end = min(extent.w, int(ceil((y + 0.5 + halfWidth - first) / pitch)));
    for (int row = start; row < end; row++) {
      float top = first + float(row) * pitch - halfWidth;
      float a = max(y - 0.5, top) - top;
      float b = min(y + 0.5, top + width) - top;
      float coverage = max(0.0, b - a);
      if (mode == 1 && coverage > 0.0) {
        if (a < feather) {
          float c = feather - a, d = max(0.0, feather - b);
          coverage -= (c * c - d * d) / (2.0 * feather);
        }
        if (b > width - feather) {
          float c = b - width + feather, d = max(0.0, a - width + feather);
          coverage -= (c * c - d * d) / (2.0 * feather);
        }
      }
      vec4 candidate = colourAt(valueAt(x, row));
      candidate.a = floor(candidate.a * coverage + 0.5);
      if (candidate.a > best.a) best = candidate;
    }
  }
  float alpha = best.a / 255.0;
  colour = vec4(best.rgb * alpha, alpha);
}`;

export interface DensityTextureFrame {
  raster: Float32Array;
  columns: number;
  bins: number;
  rows: number;
  top: number;
  height: number;
  centre: number;
  pitch: number;
  width: number;
  soft: boolean;
  probability: boolean;
  palette: DensityPalette;
  bounds: DensityRasterBounds;
  changed: boolean;
}

/** Colour and expand the numeric history on the GPU; no screen-sized RGBA upload. */
export class DensityTexture {
  readonly canvas = document.createElement('canvas');
  private gl: WebGL2RenderingContext | null;
  private program: WebGLProgram | null = null;
  private buffer: WebGLBuffer | null = null;
  private raster: WebGLTexture | null = null;
  private palette: WebGLTexture | null = null;
  private uniforms: Record<string, WebGLUniformLocation | null> = {};
  private columns = 0;
  private bins = 0;
  private colours: DensityPalette | null = null;
  private source: Float32Array | null = null;
  private first = 0;
  private end = 0;
  private limit = 0;
  private failed = false;

  constructor() {
    this.gl = this.canvas.getContext('webgl2', {
      alpha: true, premultipliedAlpha: true, antialias: false, depth: false, stencil: false,
      preserveDrawingBuffer: false, failIfMajorPerformanceCaveat: true,
    });
    if (!this.gl?.createShader) this.gl = null;
    this.canvas.addEventListener('webglcontextlost', event => {
      event.preventDefault(); this.reset();
    });
    this.canvas.addEventListener('webglcontextrestored', () => { this.failed = false; });
  }

  private reset(): void {
    this.program = null; this.buffer = null; this.raster = null; this.palette = null;
    this.source = null; this.colours = null; this.columns = this.bins = 0;
    this.first = this.end = 0;
  }

  private init(): boolean {
    const gl = this.gl;
    if (!gl || this.failed || gl.isContextLost()) return false;
    if (this.program) return true;
    const vs = gl.createShader(gl.VERTEX_SHADER), fs = gl.createShader(gl.FRAGMENT_SHADER);
    const program = gl.createProgram();
    if (!vs || !fs || !program) {
      gl.deleteShader(vs); gl.deleteShader(fs); gl.deleteProgram(program);
      this.failed = true; return false;
    }
    gl.shaderSource(vs, vertex); gl.compileShader(vs);
    gl.shaderSource(fs, fragment); gl.compileShader(fs);
    gl.attachShader(program, vs); gl.attachShader(program, fs); gl.linkProgram(program);
    gl.deleteShader(vs); gl.deleteShader(fs);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      gl.deleteProgram(program); this.failed = true; return false;
    }
    this.program = program;
    this.limit = gl.getParameter(gl.MAX_TEXTURE_SIZE);
    this.buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0); gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    gl.useProgram(program);
    for (const name of ['raster', 'palette', 'geometry', 'extent', 'outputHeight', 'maxIndex', 'mode'])
      this.uniforms[name] = gl.getUniformLocation(program, name);
    gl.uniform1i(this.uniforms.raster, 0); gl.uniform1i(this.uniforms.palette, 1);
    gl.disable(gl.DEPTH_TEST); gl.disable(gl.BLEND); gl.disable(gl.DITHER);
    return true;
  }

  private texture(unit: number, width: number, height: number, format: number): WebGLTexture | null {
    const gl = this.gl!;
    const texture = gl.createTexture();
    if (!texture) return null;
    gl.activeTexture(gl.TEXTURE0 + unit); gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texStorage2D(gl.TEXTURE_2D, 1, format, width, height);
    if (gl.getError() !== gl.NO_ERROR) { gl.deleteTexture(texture); return null; }
    return texture;
  }

  draw(frame: DensityTextureFrame): boolean {
    if (!this.init()) return false;
    const gl = this.gl!, f = frame;
    if (Math.max(f.columns, f.bins, f.rows) > this.limit || !(f.pitch > 0 && f.width > 0)) return false;
    const height = Math.min(f.rows, Math.ceil(f.height / 64) * 64);
    if (this.canvas.width !== f.columns) this.canvas.width = f.columns;
    if (this.canvas.height < f.height || this.canvas.height > f.rows) this.canvas.height = height;
    if (this.columns !== f.columns || this.bins !== f.bins) {
      gl.deleteTexture(this.raster);
      this.raster = this.texture(0, f.columns, f.bins, gl.R32F);
      if (!this.raster) { this.failed = true; return false; }
      this.columns = f.columns; this.bins = f.bins; this.source = null;
      this.first = this.end = 0;
    }
    if (f.changed || this.source !== f.raster) {
      gl.activeTexture(gl.TEXTURE0); gl.bindTexture(gl.TEXTURE_2D, this.raster);
      const first = this.end ? Math.min(this.first, f.bounds.firstBin) : f.bounds.firstBin;
      const end = Math.max(this.end, f.bounds.endBin);
      if (end > first) gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, first, f.columns, end - first,
        gl.RED, gl.FLOAT, f.raster.subarray(first * f.columns, end * f.columns));
      this.first = f.bounds.firstBin; this.end = f.bounds.endBin;
      this.source = f.raster;
    }
    if (this.colours !== f.palette) {
      if (!this.palette) this.palette = this.texture(1, 256, 256, gl.RGBA32F);
      if (!this.palette) { this.failed = true; return false; }
      const data = new Float32Array(65536 * 4);
      for (let i = 0; i < 65536; i++) {
        const c = f.palette.colours[i];
        data[i * 4] = (c & 255) / 255;
        data[i * 4 + 1] = (c >>> 8 & 255) / 255;
        data[i * 4 + 2] = (c >>> 16 & 255) / 255;
        data[i * 4 + 3] = f.palette.alpha[i];
      }
      gl.activeTexture(gl.TEXTURE1); gl.bindTexture(gl.TEXTURE_2D, this.palette);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, 256, 256, gl.RGBA, gl.FLOAT, data);
      this.colours = f.palette;
    }
    gl.viewport(0, 0, f.columns, this.canvas.height);
    gl.uniform4f(this.uniforms.geometry, f.centre, f.pitch, f.width, f.top);
    gl.uniform4i(this.uniforms.extent, f.columns, f.bins, f.bounds.firstBin, f.bounds.endBin);
    gl.uniform1i(this.uniforms.outputHeight, this.canvas.height);
    gl.uniform1i(this.uniforms.maxIndex, f.palette.maxIndex);
    gl.uniform1i(this.uniforms.mode, f.probability ? 2 : f.soft ? 1 : 0);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    return true;
  }

  dispose(): void {
    const gl = this.gl;
    if (gl) {
      gl.deleteTexture(this.raster); gl.deleteTexture(this.palette);
      gl.deleteBuffer(this.buffer); gl.deleteProgram(this.program);
      gl.getExtension('WEBGL_lose_context')?.loseContext();
    }
    this.reset(); this.gl = null; this.canvas.width = this.canvas.height = 0;
  }
}
