import {
  HISTORY_LENGTH,
  STAGE_FLOOR_DB,
  grNorm,
  levelNorm,
  type HistorySample,
} from '../meter'

const FLOATS_PER_SAMPLE = 4
const UNIFORM_FLOATS = 8

const SHADER = /* wgsl */ `
struct FrameParams {
  sample_count: u32,
  live: u32,
  width: f32,
  height: f32,
  ceiling_y: f32,
  pad0: f32,
  pad1: f32,
  pad2: f32,
}

struct VertexOutput {
  @builtin(position) position: vec4f,
  @location(0) uv: vec2f,
}

@group(0) @binding(0)
var<storage, read> history: array<vec4f>;

@group(0) @binding(1)
var<uniform> frame: FrameParams;

const GRID_Y = array<f32, 8>(
  0.0625, 0.125, 0.1875, 0.25, 0.375, 0.5, 0.75, 1.0
);

fn over(base: vec4f, top: vec4f) -> vec4f {
  return vec4f(mix(base.rgb, top.rgb, top.a), 1.0);
}

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
  var positions = array<vec2f, 3>(
    vec2f(-1.0, -1.0),
    vec2f( 3.0, -1.0),
    vec2f(-1.0,  3.0)
  );
  let position = positions[vertex_index];
  var output: VertexOutput;
  output.position = vec4f(position, 0.0, 1.0);
  output.uv = vec2f(
    (position.x + 1.0) * 0.5,
    1.0 - (position.y + 1.0) * 0.5
  );
  return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4f {
  let uv = clamp(input.uv, vec2f(0.0), vec2f(1.0));
  let pixel_y = 1.0 / max(frame.height, 1.0);
  var color = vec4f(0.006, 0.008, 0.010, 1.0);

  for (var tick = 0u; tick < 8u; tick += 1u) {
    if (abs(uv.y - GRID_Y[tick]) <= pixel_y * 0.55) {
      color = over(color, vec4f(1.0, 1.0, 1.0, 0.035));
    }
  }
  if (uv.y <= pixel_y * 0.75) {
    color = over(color, vec4f(1.0, 1.0, 1.0, 0.10));
  }

  let x_pixel = u32(floor(uv.x * frame.width));
  if (abs(uv.y - frame.ceiling_y) <= pixel_y * 0.65 &&
      (x_pixel % 9u) < 5u) {
    color = over(color, vec4f(0.91, 0.77, 0.35, 0.65));
  }

  if (frame.live != 0u && frame.sample_count >= 2u) {
    let last = frame.sample_count - 1u;
    let history_x = uv.x * f32(last);
    let index = min(u32(floor(history_x)), last - 1u);
    let amount = fract(history_x);
    let sample = mix(history[index], history[index + 1u], amount);

    if (uv.y >= sample.x) {
      let depth = clamp((uv.y - sample.x) / max(1.0 - sample.x, pixel_y), 0.0, 1.0);
      let top = vec4f(0.52, 0.68, 0.76, 0.55);
      let bottom = vec4f(0.12, 0.20, 0.26, 0.10);
      color = over(color, mix(top, bottom, depth));
    }
    if (abs(uv.y - sample.y) <= pixel_y * 1.15) {
      color = over(color, vec4f(0.91, 0.77, 0.35, 0.72));
    }
    if (uv.y <= sample.z) {
      color = over(color, vec4f(0.91, 0.28, 0.23, 0.72));
    }
    if (abs(uv.y - sample.z) <= pixel_y * 1.5) {
      color = over(color, vec4f(1.0, 0.42, 0.36, 1.0));
    }
  }

  return color;
}
`

export type WaveRenderer = {
  resize(width: number, height: number): void
  render(samples: HistorySample[], ceilingDb: number, live: boolean): void
  destroy(): void
}

class GpuWaveRenderer implements WaveRenderer {
  private readonly historyData = new Float32Array(
    HISTORY_LENGTH * FLOATS_PER_SAMPLE,
  )
  private readonly uniformData = new ArrayBuffer(
    UNIFORM_FLOATS * Float32Array.BYTES_PER_ELEMENT,
  )
  private readonly uniformFloats = new Float32Array(this.uniformData)
  private readonly uniformU32 = new Uint32Array(this.uniformData)
  private width = 1
  private height = 1
  private destroyed = false

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly device: GPUDevice,
    private readonly context: GPUCanvasContext,
    private readonly pipeline: GPURenderPipeline,
    private readonly bindGroup: GPUBindGroup,
    private readonly historyBuffer: GPUBuffer,
    private readonly uniformBuffer: GPUBuffer,
  ) {
    void device.lost.then(() => {
      this.destroyed = true
    })
  }

  resize(width: number, height: number) {
    const nextWidth = Math.max(1, width)
    const nextHeight = Math.max(1, height)
    if (nextWidth === this.width && nextHeight === this.height) return
    this.width = nextWidth
    this.height = nextHeight
    this.canvas.width = this.width
    this.canvas.height = this.height
  }

  render(samples: HistorySample[], ceilingDb: number, live: boolean) {
    if (this.destroyed) return
    const count = Math.min(samples.length, HISTORY_LENGTH)
    const start = samples.length - count
    for (let index = 0; index < count; index++) {
      const sample = samples[start + index]!
      const offset = index * FLOATS_PER_SAMPLE
      this.historyData[offset] = levelNorm(live ? sample.inDb : STAGE_FLOOR_DB)
      this.historyData[offset + 1] = levelNorm(
        live ? sample.rmsDb : STAGE_FLOOR_DB,
      )
      this.historyData[offset + 2] = grNorm(live ? sample.grDb : 0) * 0.52
      this.historyData[offset + 3] = 0
    }
    this.device.queue.writeBuffer(this.historyBuffer, 0, this.historyData)

    this.uniformU32[0] = count
    this.uniformU32[1] = live ? 1 : 0
    this.uniformFloats[2] = this.width
    this.uniformFloats[3] = this.height
    this.uniformFloats[4] = levelNorm(ceilingDb)
    this.device.queue.writeBuffer(this.uniformBuffer, 0, this.uniformData)

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
    pass.setBindGroup(0, this.bindGroup)
    pass.draw(3)
    pass.end()
    this.device.queue.submit([encoder.finish()])
  }

  destroy() {
    if (this.destroyed) return
    this.destroyed = true
    this.historyBuffer.destroy()
    this.uniformBuffer.destroy()
    this.context.unconfigure()
    this.device.destroy()
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
    },
    fragment: {
      module,
      entryPoint: 'fragment_main',
      targets: [{ format }],
    },
    primitive: { topology: 'triangle-list' },
  })
  const historyBuffer = device.createBuffer({
    label: 'BurnLimit history',
    size:
      HISTORY_LENGTH *
      FLOATS_PER_SAMPLE *
      Float32Array.BYTES_PER_ELEMENT,
    usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
  })
  const uniformBuffer = device.createBuffer({
    label: 'BurnLimit frame uniforms',
    size: UNIFORM_FLOATS * Float32Array.BYTES_PER_ELEMENT,
    usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
  })
  const bindGroup = device.createBindGroup({
    label: 'BurnLimit wave bind group',
    layout: pipeline.getBindGroupLayout(0),
    entries: [
      { binding: 0, resource: { buffer: historyBuffer } },
      { binding: 1, resource: { buffer: uniformBuffer } },
    ],
  })

  return new GpuWaveRenderer(
    canvas,
    device,
    context,
    pipeline,
    bindGroup,
    historyBuffer,
    uniformBuffer,
  )
}
