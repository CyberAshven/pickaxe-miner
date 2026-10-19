//! #### PR #58: shared Rust ELECTRIC proof on discrete and integrated GPUs.
//! The host independently verifies every returned nonce before a claim is built.
use super::{meets, preimage};
use crate::backend::{BackendKind, GpuDevice};
use cudarc::driver::{
    CudaContext, CudaFunction, CudaSlice, CudaStream, LaunchConfig, PushKernelArg,
};
use std::sync::Arc;

pub const MAX_BATCH: u32 = 1 << 20;

/// Keeps device allocations across baton updates. Work is assigned before hashing.
pub fn search_all(
    engines: &mut [Engine],
    job: super::SearchJob,
    schedule: &std::sync::Mutex<crate::donation::Schedule>,
    next_nonce: &std::sync::atomic::AtomicU64,
    stop: &std::sync::atomic::AtomicBool,
    monitor: &super::ui::Monitor,
) -> Result<Option<super::Win>, String> {
    use std::sync::atomic::Ordering::Relaxed;
    std::thread::scope(|scope| {
        let handles: Vec<_> = engines
            .iter_mut()
            .enumerate()
            .map(|(device, engine)| {
                scope.spawn(move || {
                    // Cancel peer workers on every exit, including errors and unwinding.
                    struct StopOnExit<'a>(&'a std::sync::atomic::AtomicBool);
                    impl Drop for StopOnExit<'_> {
                        fn drop(&mut self) {
                            self.0.store(true, Relaxed);
                        }
                    }
                    let _stop_on_exit = StopOnExit(stop);
                    let started = std::time::Instant::now();
                    let mut pacer =
                        crate::mining_control::DutyPacer::new(std::time::Duration::ZERO);
                    while !stop.load(Relaxed) && !monitor.stopped() {
                        if monitor.paused.load(Relaxed) {
                            monitor.wait(std::time::Duration::from_millis(50));
                            pacer.reset(started.elapsed());
                            continue;
                        }
                        // #### PR #60: another token's turn on the GPUs.
                        if monitor.held.load(std::sync::atomic::Ordering::Acquire) {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            pacer.reset(started.elapsed());
                            continue;
                        }
                        let (recipient, count) = {
                            let mut schedule = schedule.lock().unwrap_or_else(|e| e.into_inner());
                            let recipient = schedule.recipient();
                            let count = schedule.limit_batch(MAX_BATCH);
                            schedule.record(count)?;
                            (recipient, count)
                        };
                        let base = next_nonce.fetch_add(u64::from(count), Relaxed);
                        let pkh = job.pkhs[recipient as usize];
                        let batch_start = std::time::Instant::now();
                        let result =
                            engine.search(job.last_height, &pkh, job.difficulty, base, count);
                        if result.is_err() {
                            stop.store(true, Relaxed);
                        }
                        let result = result?;
                        let elapsed = batch_start.elapsed();
                        monitor.record(device, count, elapsed);
                        if let Some(nonce) = result {
                            stop.store(true, Relaxed);
                            return Ok(Some(super::Win {
                                recipient,
                                pkh,
                                nonce,
                            }));
                        }
                        monitor.wait(pacer.record_batch(
                            monitor.intensity.load(Relaxed),
                            elapsed,
                            started.elapsed(),
                        ));
                    }
                    Ok(None)
                })
            })
            .collect();
        let mut win = None;
        let mut error = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(Some(found))) if win.is_none() => win = Some(found),
                Ok(Err(e)) => error = Some(e),
                Err(_) => {
                    stop.store(true, Relaxed);
                    error = Some("ELECTRIC GPU worker panicked".to_string());
                }
                _ => {}
            }
        }
        // A completed verified proof is still useful even if another GPU failed.
        if win.is_some() {
            Ok(win)
        } else if let Some(error) = error {
            Err(error)
        } else {
            Ok(None)
        }
    })
}

pub enum Engine {
    Cuda(Cuda),
    #[cfg(feature = "portable-wgpu")]
    Wgpu(Portable),
}

impl Engine {
    pub fn new(gpu: &GpuDevice) -> Result<Self, String> {
        match gpu.backend {
            BackendKind::Cuda => Cuda::new(gpu.index as usize).map(Self::Cuda),
            #[cfg(feature = "portable-wgpu")]
            BackendKind::Wgpu => {
                pollster::block_on(Portable::new(gpu.index as usize)).map(Self::Wgpu)
            }
            _ => Err("ELECTRIC needs a CUDA or portable wgpu build".into()),
        }
    }

    pub fn search(
        &mut self,
        height: u32,
        pkh: &[u8; 20],
        difficulty: u8,
        base: u64,
        count: u32,
    ) -> Result<Option<u64>, String> {
        let control = control(height, pkh, difficulty, base, count)?;
        let result = self.dispatch(&control, false)?;
        if result.len() != 2 || result[1] != count {
            return Err("ELECTRIC GPU did not complete the allocated work".into());
        }
        if result[0] == u32::MAX {
            return Ok(None);
        }
        let nonce = base.wrapping_add(u64::from(result[0]));
        if result[0] >= count
            || !meets(
                &crate::proof::hash256(&preimage(height, pkh, nonce)),
                difficulty,
            )
        {
            return Err("ELECTRIC GPU returned an invalid proof".into());
        }
        Ok(Some(nonce))
    }

    fn dispatch(&mut self, control: &[u32; 10], probe: bool) -> Result<Vec<u32>, String> {
        match self {
            Self::Cuda(engine) => engine.dispatch(control, probe),
            #[cfg(feature = "portable-wgpu")]
            Self::Wgpu(engine) => engine.dispatch(control, probe),
        }
    }
}

fn control(
    height: u32,
    pkh: &[u8; 20],
    difficulty: u8,
    base: u64,
    count: u32,
) -> Result<[u32; 10], String> {
    if count == 0 || count > MAX_BATCH || difficulty > 32 {
        return Err("invalid ELECTRIC GPU work range or difficulty".into());
    }
    let bytes = preimage(height, pkh, base);
    let mut control = [0; 10];
    for (word, bytes) in control[..6].iter_mut().zip(bytes[..24].as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    control[6] = base as u32;
    control[7] = (base >> 32) as u32;
    control[8] = count;
    control[9] = u32::from(difficulty);
    Ok(control)
}

/// ELECTRIC's CUDA module (`tools/build-rust-kernels`), beside PHOTON's.
const PTX: &str = "electric_rust.ptx";

pub struct Cuda {
    stream: Arc<CudaStream>,
    filter: CudaFunction,
    probe: CudaFunction,
    control: CudaSlice<u32>,
    result: CudaSlice<u32>,
}

impl Cuda {
    fn new(ordinal: usize) -> Result<Self, String> {
        let context = CudaContext::new(ordinal).map_err(|e| e.to_string())?;
        let stream = context.default_stream();
        // #### PR #58: ELECTRIC's own module, loaded here: PHOTON's loader
        // maps every module name of the Rust T2 build to photon_rust.ptx.
        let path = crate::cuda_photon::resolve_ptx_path(PTX)?;
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let module = context
            .load_module(cudarc::nvrtc::Ptx::from_src(source))
            .map_err(|error| format!("load {PTX}: {error}"))?;
        let load = |name: &str| {
            module
                .load_function(name)
                .map_err(|error| format!("load {name} from {PTX}: {error}"))
        };
        let filter = load("pickaxe_electric_filter")?;
        let probe = load("pickaxe_electric_probe")?;
        Ok(Self {
            control: stream.alloc_zeros(10).map_err(|e| e.to_string())?,
            result: stream.alloc_zeros(2).map_err(|e| e.to_string())?,
            stream,
            filter,
            probe,
        })
    }

    fn dispatch(&mut self, control: &[u32; 10], probe: bool) -> Result<Vec<u32>, String> {
        self.stream
            .memcpy_htod(control, &mut self.control)
            .map_err(|e| e.to_string())?;
        let mut probe_output = if probe {
            Some(
                self.stream
                    .alloc_zeros::<u32>(control[8] as usize * 8)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let output = probe_output.as_mut().unwrap_or(&mut self.result);
        if !probe {
            self.stream
                .memcpy_htod(&[u32::MAX, 0], output)
                .map_err(|e| e.to_string())?;
        }
        let mut launch = self
            .stream
            .launch_builder(if probe { &self.probe } else { &self.filter });
        launch.arg(&self.control).arg(&mut *output);
        // Host bounds and allocation cover every invocation; readback synchronizes.
        unsafe {
            launch.launch(LaunchConfig {
                grid_dim: (control[8].div_ceil(128), 1, 1),
                block_dim: (128, 1, 1),
                shared_mem_bytes: 0,
            })
        }
        .map_err(|e| e.to_string())?;
        self.stream.clone_dtoh(output).map_err(|e| e.to_string())
    }
}

#[cfg(feature = "portable-wgpu")]
pub struct Portable {
    device: wgpu::Device,
    queue: wgpu::Queue,
    filter: wgpu::ComputePipeline,
    probe: wgpu::ComputePipeline,
    control: wgpu::Buffer,
    result: wgpu::Buffer,
    readback: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

#[cfg(feature = "portable-wgpu")]
impl Portable {
    async fn new(ordinal: usize) -> Result<Self, String> {
        use crate::wgpu_photon::{
            is_hardware_adapter, production_instance_descriptor, production_wgpu_backends,
        };
        let backends = production_wgpu_backends()?;
        let instance = wgpu::Instance::new(production_instance_descriptor(backends)?);
        let mut adapters = instance
            .enumerate_adapters(backends)
            .await
            .into_iter()
            .filter(|adapter| is_hardware_adapter(&adapter.get_info()));
        #[cfg(test)]
        let named = std::env::var("PICKAXE_TEST_WGPU_ADAPTER").ok();
        #[cfg(not(test))]
        let named: Option<String> = None;
        let adapter = match named {
            Some(name) => adapters.find(|adapter| adapter.get_info().name.contains(&name)),
            None => adapters.nth(ordinal),
        }
        .ok_or("ELECTRIC: no selected hardware WGPU adapter")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("ELECTRIC"),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shared Rust ELECTRIC"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../reference/shared-electric/pickaxe_electric.wgsl").into(),
            ),
        });
        let pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let filter = pipeline("pickaxe_electric_filter");
        let probe = pipeline("pickaxe_electric_probe");
        if let Some(error) = error_scope.pop().await {
            return Err(error.to_string());
        }
        let control = buffer(
            &device,
            40,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let result = buffer(
            &device,
            8,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
        );
        let readback = buffer(
            &device,
            8,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let bind = binding(&device, &filter, &control, &result);
        Ok(Self {
            device,
            queue,
            filter,
            probe,
            control,
            result,
            readback,
            bind,
        })
    }

    fn dispatch(&mut self, control: &[u32; 10], probe: bool) -> Result<Vec<u32>, String> {
        let bytes: Vec<_> = control.iter().flat_map(|word| word.to_le_bytes()).collect();
        self.queue.write_buffer(&self.control, 0, &bytes);
        let size = if probe { u64::from(control[8]) * 32 } else { 8 };
        let probe_buffers = probe.then(|| {
            (
                buffer(
                    &self.device,
                    size,
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                ),
                buffer(
                    &self.device,
                    size,
                    wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                ),
            )
        });
        let (output, readback) = probe_buffers
            .as_ref()
            .map_or((&self.result, &self.readback), |(a, b)| (a, b));
        let probe_bind = probe.then(|| binding(&self.device, &self.probe, &self.control, output));
        if !probe {
            self.queue
                .write_buffer(output, 0, &[255, 255, 255, 255, 0, 0, 0, 0]);
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(if probe { &self.probe } else { &self.filter });
            pass.set_bind_group(0, probe_bind.as_ref().unwrap_or(&self.bind), &[]);
            pass.dispatch_workgroups(control[8].div_ceil(128), 1, 1);
        }
        encoder.copy_buffer_to_buffer(output, 0, readback, 0, size);
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(std::time::Duration::from_secs(1))
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let view = slice.get_mapped_range().map_err(|e| e.to_string())?;
        let words = view
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect();
        drop(view);
        readback.unmap();
        Ok(words)
    }
}

#[cfg(feature = "portable-wgpu")]
fn buffer(device: &wgpu::Device, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ELECTRIC"),
        size,
        usage,
        mapped_at_creation: false,
    })
}

#[cfg(feature = "portable-wgpu")]
fn binding(
    device: &wgpu::Device,
    pipeline: &wgpu::ComputePipeline,
    control: &wgpu::Buffer,
    output: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ELECTRIC"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: control.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oracle(engine: &mut Engine) {
        if let Some(path) = std::env::var_os("PICKAXE_ELECTRIC_PROOF") {
            super::super::tests::write_vm_vectors(
                &std::path::PathBuf::from(path),
                |height, pkh, difficulty, base, count| {
                    (0..count).step_by(MAX_BATCH as usize).find_map(|offset| {
                        engine
                            .search(
                                height,
                                pkh,
                                difficulty,
                                base.wrapping_add(offset),
                                (count - offset).min(u64::from(MAX_BATCH)) as u32,
                            )
                            .unwrap()
                    })
                },
            );
        }
        for (height, pkh, base) in [
            (328_400, [0x11; 20], 0),
            (u32::MAX, [0xff; 20], u64::from(u32::MAX) - 127),
            (0, [0; 20], u64::MAX - 127),
            (328_401, [0x22; 20], 0x1234_5678_9abc_def0),
        ] {
            let count = 257;
            let control = control(height, &pkh, 0, base, count).unwrap();
            let actual = engine.dispatch(&control, true).unwrap();
            for i in 0..count {
                let expected =
                    crate::proof::hash256(&preimage(height, &pkh, base.wrapping_add(u64::from(i))));
                let digest: Vec<_> = actual[i as usize * 8..i as usize * 8 + 8]
                    .iter()
                    .flat_map(|word| word.to_be_bytes())
                    .collect();
                assert_eq!(digest, expected);
            }
            for difficulty in [0, 1, 2, 3, 4, 31, 32] {
                let expected =
                    super::super::search(height, &pkh, difficulty, base, u64::from(count));
                assert_eq!(
                    engine
                        .search(height, &pkh, difficulty, base, count)
                        .unwrap(),
                    expected
                );
            }
            assert!(engine.search(height, &pkh, 33, base, count).is_err());
            assert!(engine.search(height, &pkh, 3, base, 0).is_err());
            assert!(engine.search(height, &pkh, 3, base, MAX_BATCH + 1).is_err());
        }
    }

    #[test]
    #[ignore = "serial GPU test; stop live miners first"]
    fn electric_cuda_matches_independent_hashes_if_cuda_present() {
        oracle(&mut Engine::Cuda(Cuda::new(0).unwrap()));
    }

    #[cfg(feature = "portable-wgpu")]
    #[test]
    #[ignore = "serial GPU test; stop live miners first"]
    fn electric_portable_matches_independent_hashes_if_wgpu_present() {
        oracle(&mut Engine::Wgpu(
            pollster::block_on(Portable::new(0)).unwrap(),
        ));
    }
}
