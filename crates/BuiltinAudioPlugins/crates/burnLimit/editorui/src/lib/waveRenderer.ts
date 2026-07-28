import {
  STAGE_TICKS_DB,
  grNorm,
  levelNorm,
  type HistorySample,
} from '../meter'

const FLOATS_PER_VERTEX = 6
const MAX_VERTICES = 4096

const SHADER = /* wgsl */ `
struct VertexInput {
  @location(0) position: vec2f,
  @location(1) color: vec4f,
}

struct VertexOutput {
  @builtin(position) position: vec4f,
  @location(0) color: vec4f,
}

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
  var output: VertexOutput;
  output.position = vec4f(input.position, 0.0, 1.0);
  output.color = input.color;
  return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4f {
  return input.color;
}
`

type Color = readonly [number, number, number, number]

const GRID: Color = [1, 1, 1, 0.035]
const ZERO: Color = [1, 1, 1, 0.1]
const CEILING: Color = [0.91, 0.77, 0.35, 0.65]
const INPUT_TOP: Color = [0.52, 0.68, 0.76, 0.55]
const INPUT_BOTTOM: Color = [0.12, 0.2, 0.26, 0.1]
const RMS: Color = [0.91, 0.77, 0.35, 0.72]
const GR_FILL: Color = [0.91, 0.28, 0.23, 0.72]
const GR_LINE: Color = [1, 0.42, 0.36, 1]

export type WaveRenderer = {
  resize(width: number, height: number): void
  render(samples: HistorySample[], ceilingDb: number, live: boolean): void
  destroy(): void
}

class GpuWaveRenderer implements WaveRenderer {
  private readonly vertices = new Float32Array(
    MAX_VERTICES * FLOATS_PER_VERTEX,
  )
  private vertexCount = 0
  private width = 1
  private height = 1
  private destroyed = false

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly device: GPUDevice,
    private readonly context: GPUCanvasContext,
    private readonly pipeline: GPURenderPipeline,
    private readonly vertexBuffer: GPUBuffer,
  ) {}

  resize(width: number, height: number) {
    this.width = Math.max(1, width)
    this.height = Math.max(1, height)
    this.canvas.width = this.width
    this.canvas.height = this.height
  }

  render(samples: HistorySample[], ceilingDb: number, live: boolean) {
    if (this.destroyed) return
    this.vertexCount = 0

    for (const tick of STAGE_TICKS_DB) {
      if (tick === 0) continue
      this.line(0, levelNorm(tick), 1, levelNorm(tick), 1, GRID)
    }
    this.line(0, 0, 1, 0, 1, ZERO)
    this.dashedLine(levelNorm(ceilingDb), CEILING)

    if (samples.length >= 2) {
      const denominator = Math.max(samples.length - 1, 1)
      for (let index = 0; index < samples.length - 1; index++) {
        const next = index + 1
        const x0 = index / denominator
        const x1 = next / denominator
        const input0 = levelNorm(live ? samples[index]!.inDb : -48)
        const input1 = levelNorm(live ? samples[next]!.inDb : -48)
        this.area(x0, input0, x1, input1, 1, INPUT_TOP, INPUT_BOTTOM)

        const gr0 = grNorm(live ? samples[index]!.grDb : 0) * 0.52
        const gr1 = grNorm(live ? samples[next]!.grDb : 0) * 0.52
        this.area(x0, gr0, x1, gr1, 0, GR_FILL, GR_FILL)
      }

      for (let index = 0; index < samples.length - 1; index++) {
        const next = index + 1
        const x0 = index / denominator
        const x1 = next / denominator
        this.line(
          x0,
          levelNorm(live ? samples[index]!.rmsDb : -48),
          x1,
          levelNorm(live ? samples[next]!.rmsDb : -48),
          1.15,
          RMS,
        )
        this.line(
          x0,
          grNorm(live ? samples[index]!.grDb : 0) * 0.52,
          x1,
          grNorm(live ? samples[next]!.grDb : 0) * 0.52,
          1.5,
          GR_LINE,
        )
      }
    }

    const used = this.vertices.subarray(
      0,
      this.vertexCount * FLOATS_PER_VERTEX,
    )
    this.device.queue.writeBuffer(this.vertexBuffer, 0, used)

    const encoder = this.device.createCommandEncoder({
      label: 'BurnLimit wave encoder',
    })
    const pass = encoder.beginRenderPass({
      label: 'BurnLimit wave pass',
      colorAttachments: [
        {
          view: this.context.getCurrentTexture().createView(),
          clearValue: { r: 0.006, g: 0.008, b: 0.01, a: 1 },
          loadOp: 'clear',
          storeOp: 'store',
        },
      ],
    })
    pass.setPipeline(this.pipeline)
    pass.setVertexBuffer(0, this.vertexBuffer)
    pass.draw(this.vertexCount)
    pass.end()
    this.device.queue.submit([encoder.finish()])
  }

  destroy() {
    this.destroyed = true
    this.vertexBuffer.destroy()
  }

  private vertex(x: number, y: number, color: Color) {
    if (this.vertexCount >= MAX_VERTICES) return
    const offset = this.vertexCount * FLOATS_PER_VERTEX
    this.vertices[offset] = x * 2 - 1
    this.vertices[offset + 1] = 1 - y * 2
    this.vertices[offset + 2] = color[0]
    this.vertices[offset + 3] = color[1]
    this.vertices[offset + 4] = color[2]
    this.vertices[offset + 5] = color[3]
    this.vertexCount += 1
  }

  private triangle(
    a: readonly [number, number],
    b: readonly [number, number],
    c: readonly [number, number],
    colors: readonly [Color, Color, Color],
  ) {
    this.vertex(a[0], a[1], colors[0])
    this.vertex(b[0], b[1], colors[1])
    this.vertex(c[0], c[1], colors[2])
  }

  private area(
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    baseline: number,
    curveColor: Color,
    baseColor: Color,
    curve0 = y0,
    curve1 = y1,
  ) {
    this.triangle(
      [x0, curve0],
      [x0, baseline],
      [x1, baseline],
      [curveColor, baseColor, baseColor],
    )
    this.triangle(
      [x0, curve0],
      [x1, baseline],
      [x1, curve1],
      [curveColor, baseColor, curveColor],
    )
  }

  private line(
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    thicknessPx: number,
    color: Color,
  ) {
    const dx = (x1 - x0) * this.width
    const dy = (y1 - y0) * this.height
    const length = Math.hypot(dx, dy)
    if (length <= Number.EPSILON) return
    const half = thicknessPx * 0.5
    const ox = (-dy / length) * (half / this.width)
    const oy = (dx / length) * (half / this.height)
    this.triangle(
      [x0 + ox, y0 + oy],
      [x0 - ox, y0 - oy],
      [x1 - ox, y1 - oy],
      [color, color, color],
    )
    this.triangle(
      [x0 + ox, y0 + oy],
      [x1 - ox, y1 - oy],
      [x1 + ox, y1 + oy],
      [color, color, color],
    )
  }

  private dashedLine(y: number, color: Color) {
    const dashPx = 5
    const gapPx = 4
    for (let x = 0; x < this.width; x += dashPx + gapPx) {
      this.line(
        x / this.width,
        y,
        Math.min(x + dashPx, this.width) / this.width,
        y,
        1,
        color,
      )
    }
  }
}

export async function createWaveRenderer(
  canvas: HTMLCanvasElement,
): Promise<WaveRenderer> {
  if (!navigator.gpu) throw new Error('WebGPU is unavailable')
  const adapter = await navigator.gpu.requestAdapter({
    powerPreference: 'high-performance',
  })
  if (!adapter) throw new Error('No WebGPU adapter')
  const device = await adapter.requestDevice()
  const context = canvas.getContext('webgpu')
  if (!context) throw new Error('Unable to create a WebGPU canvas context')

  const format = navigator.gpu.getPreferredCanvasFormat()
  context.configure({ device, format, alphaMode: 'opaque' })

  const module = device.createShaderModule({
    label: 'BurnLimit wave shader',
    code: SHADER,
  })
  const pipeline = device.createRenderPipeline({
    label: 'BurnLimit wave pipeline',
    layout: 'auto',
    vertex: {
      module,
      entryPoint: 'vertex_main',
      buffers: [
        {
          arrayStride: FLOATS_PER_VERTEX * Float32Array.BYTES_PER_ELEMENT,
          attributes: [
            { shaderLocation: 0, offset: 0, format: 'float32x2' },
            {
              shaderLocation: 1,
              offset: 2 * Float32Array.BYTES_PER_ELEMENT,
              format: 'float32x4',
            },
          ],
        },
      ],
    },
    fragment: {
      module,
      entryPoint: 'fragment_main',
      targets: [
        {
          format,
          blend: {
            color: {
              srcFactor: 'src-alpha',
              dstFactor: 'one-minus-src-alpha',
            },
            alpha: {
              srcFactor: 'one',
              dstFactor: 'one-minus-src-alpha',
            },
          },
        },
      ],
    },
    primitive: { topology: 'triangle-list' },
  })
  const vertexBuffer = device.createBuffer({
    label: 'BurnLimit wave vertices',
    size:
      MAX_VERTICES *
      FLOATS_PER_VERTEX *
      Float32Array.BYTES_PER_ELEMENT,
    usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
  })

  return new GpuWaveRenderer(canvas, device, context, pipeline, vertexBuffer)
}
