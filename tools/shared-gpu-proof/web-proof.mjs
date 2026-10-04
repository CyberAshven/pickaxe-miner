// Offline validation only; run explicitly while other GPU consumers are stopped.
export async function verifySharedHash({ assembly = false, onProgress = () => {} } = {}) {
  if (!navigator.gpu) throw new Error('WebGPU unavailable');
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'high-performance' });
  if (!adapter) throw new Error('No GPU adapter');
  const device = await adapter.requestDevice();
  const buffers = [];
  try {
    const fetchFile = async (name, text = false) => {
      const response = await fetch(name, { cache: 'no-store' });
      if (!response.ok) throw new Error(`Missing fixture: ${name}`);
      return text ? response.text() : response.arrayBuffer();
    };
    const stem = assembly ? 'assembly-' : '';
    const outputWords = assembly ? 48 : 9;
    const inputWords = assembly ? 176 : 128;
    const entryPoint = assembly ? 'pickaxe_t2_assembly_proof' : 'pickaxe_t2_hash_proof';
    onProgress('Loading fixtures');
    const [code, inputBytes, expectedBytes] = await Promise.all([
      fetchFile('./shared-t2-hash.wgsl', true),
      fetchFile(`./${stem}inputs.bin`),
      fetchFile(`./${stem}expected.bin`),
    ]);
    const count = expectedBytes.byteLength / (outputWords * 4);
    if (!Number.isInteger(count) || inputBytes.byteLength !== count * inputWords * 4) {
      throw new Error('Fixture sizes do not match');
    }
    const buffer = (size, usage) => {
      const result = device.createBuffer({ size, usage });
      buffers.push(result);
      return result;
    };
    const input = buffer(inputBytes.byteLength, GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST);
    const output = buffer(expectedBytes.byteLength, GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC);
    const readback = buffer(expectedBytes.byteLength, GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ);
    device.queue.writeBuffer(input, 0, inputBytes);
    onProgress('Compiling shader module');
    const shader = device.createShaderModule({ code });
    const errors = (await shader.getCompilationInfo()).messages.filter(message => message.type === 'error');
    if (errors.length) throw new Error(errors.map(message => message.message).join('\n'));
    device.pushErrorScope('validation');
    onProgress('Compiling pipeline');
    const pipeline = await device.createComputePipelineAsync({
      layout: 'auto', compute: { module: shader, entryPoint },
    });
    const binding = device.createBindGroup({ layout: pipeline.getBindGroupLayout(0), entries: [
      { binding: 0, resource: { buffer: input } },
      { binding: 1, resource: { buffer: output } },
    ] });
    const commands = device.createCommandEncoder();
    const pass = commands.beginComputePass();
    pass.setPipeline(pipeline);
    pass.setBindGroup(0, binding);
    pass.dispatchWorkgroups(Math.ceil(count / 64) + 1);
    pass.end();
    commands.copyBufferToBuffer(output, 0, readback, 0, expectedBytes.byteLength);
    onProgress('Executing GPU oracle');
    device.queue.submit([commands.finish()]);
    await readback.mapAsync(GPUMapMode.READ);
    const actual = new Uint32Array(readback.getMappedRange().slice(0));
    readback.unmap();
    const validation = await device.popErrorScope();
    if (validation) throw new Error(validation.message);
    const expected = new Uint32Array(expectedBytes);
    for (let index = 0; index < expected.length; index++) {
      if (actual[index] !== expected[index]) {
        throw new Error(`Mismatch at case ${Math.floor(index / outputWords)}, word ${index % outputWords}`);
      }
    }
    return { cases: count, mismatches: 0, adapter: adapter.info.description, vendor: adapter.info.vendor };
  } finally {
    for (const buffer of buffers) buffer.destroy();
    device.destroy();
  }
}
