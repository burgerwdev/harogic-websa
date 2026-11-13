const vertex = `#version 300 es
layout(location = 0) in vec4 segment;
uniform vec2 size;
uniform float halfWidth;
uniform float originY;
flat out vec2 start;
flat out vec2 finish;
void main() {
  vec2 corners[6] = vec2[6](vec2(0,-1),vec2(1,-1),vec2(0,1),vec2(0,1),vec2(1,-1),vec2(1,1));
  vec2 corner = corners[gl_VertexID];
  start = segment.xy; finish = segment.zw;
  vec2 delta = finish - start;
  vec2 direction = delta / max(length(delta), 0.0001);
  vec2 normal = vec2(-direction.y, direction.x);
  vec2 point = mix(start, finish, corner.x)
    + (normal * corner.y + direction * (corner.x * 2.0 - 1.0)) * (halfWidth + 0.5);
  point.y -= originY;
  gl_Position = vec4(point / size * vec2(2,-2) + vec2(-1,1), 0, 1);
}`;
const fragment = `#version 300 es
precision highp float;
uniform vec2 size;
uniform float halfWidth;
uniform float originY;
uniform vec4 ink;
flat in vec2 start;
flat in vec2 finish;
out vec4 colour;
void main() {
  vec2 point = vec2(gl_FragCoord.x, size.y - gl_FragCoord.y + originY);
  vec2 delta = finish - start;
  float t = clamp(dot(point - start, delta) / max(dot(delta, delta), 0.00001), 0.0, 1.0);
  float coverage = clamp(halfWidth + 0.5 - length(point - mix(start, finish, t)), 0.0, 1.0);
  float alpha = coverage * ink.a;
  colour = vec4(ink.rgb * alpha, alpha);
}`;

type Plot = { x: number; y: number; w: number; h: number };

/** Batched antialiased segments avoid Canvas2D's costly dense-path rasterisation. */
export class TraceTexture {
  private canvas = document.createElement('canvas');
  private gl: WebGL2RenderingContext | null;
  private program: WebGLProgram | null = null;
  private buffer: WebGLBuffer | null = null;
  private uniforms: Record<string, WebGLUniformLocation | null> = {};
  private vertices = new Float32Array(0);
  private used = 0;
  private capacity = 0;
  private uploaded = false;
  private failed = false;
  private limit = 0;
  private ratio = 1;
  private plot: Plot = { x: 0, y: 0, w: 1, h: 1 };
  private x = 0;
  private y = 0;
  private top = Infinity;
  private bottom = -Infinity;
  private drawTop = 0;
  private drawHeight = 0;

  constructor() {
    this.gl = this.canvas.getContext('webgl2', {
      alpha: true, premultipliedAlpha: true, antialias: false, depth: false, stencil: false,
      preserveDrawingBuffer: false, failIfMajorPerformanceCaveat: true,
    });
    if (!this.gl?.createShader) this.gl = null;
    this.canvas.addEventListener('webglcontextlost', event => {
      event.preventDefault(); this.program = this.buffer = null; this.capacity = 0;
    });
    this.canvas.addEventListener('webglcontextrestored', () => { this.failed = false; });
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
    this.program = program; this.buffer = gl.createBuffer();
    this.limit = gl.getParameter(gl.MAX_TEXTURE_SIZE);
    gl.useProgram(program); gl.bindBuffer(gl.ARRAY_BUFFER, this.buffer);
    gl.enableVertexAttribArray(0); gl.vertexAttribPointer(0, 4, gl.FLOAT, false, 0, 0);
    gl.vertexAttribDivisor(0, 1);
    for (const name of ['size', 'halfWidth', 'originY', 'ink']) this.uniforms[name] = gl.getUniformLocation(program, name);
    gl.disable(gl.DEPTH_TEST); gl.disable(gl.DITHER);
    gl.enable(gl.BLEND); gl.blendEquation(gl.MAX); gl.blendFunc(gl.ONE, gl.ONE);
    return true;
  }

  begin(plot: Plot, ratio: number): boolean {
    if (!this.init()) return false;
    const w = Math.ceil(plot.w * ratio), h = Math.ceil(plot.h * ratio);
    if (!(w > 0 && h > 0) || Math.max(w, h) > this.limit) return false;
    this.plot = plot; this.ratio = ratio; this.used = 0; this.uploaded = false;
    this.top = Infinity; this.bottom = -Infinity;
    this.drawHeight = 0;
    return true;
  }

  moveTo(x: number, y: number): void {
    this.x = (x - this.plot.x) * this.ratio; this.y = (y - this.plot.y) * this.ratio;
  }

  lineTo(x: number, y: number): void {
    const nextX = (x - this.plot.x) * this.ratio, nextY = (y - this.plot.y) * this.ratio;
    if (this.used + 4 > this.vertices.length) {
      const larger = new Float32Array(Math.max(4096, this.vertices.length * 2));
      larger.set(this.vertices); this.vertices = larger;
    }
    this.vertices[this.used++] = this.x; this.vertices[this.used++] = this.y;
    this.vertices[this.used++] = nextX; this.vertices[this.used++] = nextY;
    this.top = Math.min(this.top, this.y, nextY); this.bottom = Math.max(this.bottom, this.y, nextY);
    this.x = nextX; this.y = nextY;
  }

  stroke(colour: string, width: number, alpha = 1): void {
    if (!this.used) return;
    const gl = this.gl!;
    if (!this.uploaded) {
      const w = Math.ceil(this.plot.w * this.ratio), h = Math.ceil(this.plot.h * this.ratio);
      const margin = width * this.ratio / 2 + 1;
      this.drawTop = Math.max(0, Math.floor(this.top - margin));
      this.drawHeight = Math.min(h, Math.ceil(this.bottom + margin)) - this.drawTop;
      if (!(this.drawHeight > 0)) return;
      if (this.canvas.width !== w) this.canvas.width = w;
      if (this.canvas.height < this.drawHeight || this.canvas.height > h)
        this.canvas.height = Math.min(h, Math.ceil(this.drawHeight / 64) * 64);
      gl.viewport(0, 0, w, this.canvas.height); gl.uniform2f(this.uniforms.size, w, this.canvas.height);
      gl.uniform1f(this.uniforms.originY, this.drawTop);
      gl.clearColor(0, 0, 0, 0); gl.clear(gl.COLOR_BUFFER_BIT);
      if (this.capacity < this.vertices.length) {
        gl.bufferData(gl.ARRAY_BUFFER, this.vertices.byteLength, gl.DYNAMIC_DRAW);
        this.capacity = this.vertices.length;
      }
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, this.vertices.subarray(0, this.used));
      this.uploaded = true;
    }
    const hex = colour.length === 4 ? colour.slice(1).split('').map(c => c + c).join('') : colour.slice(1);
    const rgb = Number.parseInt(hex, 16);
    gl.uniform4f(this.uniforms.ink, (rgb >>> 16 & 255) / 255, (rgb >>> 8 & 255) / 255, (rgb & 255) / 255, alpha);
    gl.uniform1f(this.uniforms.halfWidth, width * this.ratio / 2);
    gl.drawArraysInstanced(gl.TRIANGLES, 0, 6, this.used / 4);
  }

  composite(target: CanvasRenderingContext2D): void {
    if (!(this.drawHeight > 0)) return;
    target.save(); target.imageSmoothingEnabled = false;
    target.drawImage(this.canvas, 0, 0, this.canvas.width, this.drawHeight,
      this.plot.x, this.plot.y + this.drawTop / this.ratio, this.canvas.width / this.ratio, this.drawHeight / this.ratio);
    target.restore();
  }

  dispose(): void {
    const gl = this.gl;
    if (gl) {
      gl.deleteBuffer(this.buffer); gl.deleteProgram(this.program);
      gl.getExtension('WEBGL_lose_context')?.loseContext();
    }
    this.gl = null; this.program = this.buffer = null; this.vertices = new Float32Array(0);
    this.canvas.width = this.canvas.height = 0;
  }
}
