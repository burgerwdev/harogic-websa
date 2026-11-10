/** One bounded bitmap for static canvas content, invalidated by geometry/theme keys. */
export class CanvasLayer {
  private canvas: HTMLCanvasElement | null = null;
  private context: CanvasRenderingContext2D | null = null;
  private key = '';
  draw(target: CanvasRenderingContext2D, key: string, x: number, y: number, width: number, height: number,
    ratio: number, paint: (context: CanvasRenderingContext2D) => void): boolean {
    if (!(width > 0 && height > 0 && ratio > 0)) return false;
    if (!this.canvas) {
      this.canvas = document.createElement('canvas'); this.context = this.canvas.getContext('2d');
    }
    if (!this.context) return false;
    const w = Math.ceil(width * ratio), h = Math.ceil(height * ratio), fullKey = `${key}:${w}:${h}:${ratio}`;
    if (this.key !== fullKey) {
      if (this.canvas.width !== w) this.canvas.width = w;
      if (this.canvas.height !== h) this.canvas.height = h;
      this.context.setTransform(ratio, 0, 0, ratio, 0, 0);
      this.context.clearRect(0, 0, width, height); paint(this.context); this.key = fullKey;
    }
    target.drawImage(this.canvas, 0, 0, w, h, x, y, w / ratio, h / ratio);
    return true;
  }
}
