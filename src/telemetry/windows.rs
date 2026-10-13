//! #### PR #48: PCI-matched WDDM telemetry when a vendor utility is unavailable.
//! Only utilization and dedicated memory are reported; power/heat stay unknown.

use super::GpuTelemetry;
use crate::backend::PciAddress;
use std::{collections::BTreeMap, mem::size_of, ptr};
use windows_sys::{Wdk::Graphics::Direct3D::*, Win32::System::Performance::*};

pub(super) struct Sampler {
    query: PDH_HQUERY,
    engines: PDH_HCOUNTER,
    memory: PDH_HCOUNTER,
    adapters: BTreeMap<PciAddress, Option<String>>,
}

impl Sampler {
    pub fn new() -> Option<Self> {
        let mut query = ptr::null_mut();
        // SAFETY: all output pointers refer to initialized, correctly sized storage.
        if unsafe { PdhOpenQueryW(ptr::null(), 0, &mut query) } != 0 {
            return None;
        }
        let mut sampler = Self {
            query,
            engines: ptr::null_mut(),
            memory: ptr::null_mut(),
            adapters: adapters(),
        };
        for (path, counter) in [
            (
                r"\GPU Engine(*)\Utilization Percentage",
                &mut sampler.engines,
            ),
            (
                r"\GPU Adapter Memory(*)\Dedicated Usage",
                &mut sampler.memory,
            ),
        ] {
            let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
            // English names work on localized Windows; query owns these counters.
            if unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, counter) } != 0 {
                return None;
            }
        }
        // Rate counters need a previous observation; the worker supplies the delay.
        unsafe { PdhCollectQueryData(query) };
        Some(sampler)
    }

    pub fn sample(&mut self) -> BTreeMap<PciAddress, GpuTelemetry> {
        if unsafe { PdhCollectQueryData(self.query) } != 0 {
            return BTreeMap::new();
        }
        let engines = counter_values(self.engines);
        let memory = counter_values(self.memory);
        self.adapters
            .iter()
            .filter_map(|(pci, id)| {
                metrics(id.as_deref()?, &engines, &memory).map(|sample| (*pci, sample))
            })
            .collect()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}

struct Adapter(u32);
impl Drop for Adapter {
    fn drop(&mut self) {
        unsafe { D3DKMTCloseAdapter(&D3DKMT_CLOSEADAPTER { hAdapter: self.0 }) };
    }
}

fn adapters() -> BTreeMap<PciAddress, Option<String>> {
    let mut result = BTreeMap::new();
    let mut request = D3DKMT_ENUMADAPTERS2::default();
    // SAFETY: the count-only query does not open adapters. The second call fills
    // the bounded array; each returned handle is closed even on query failure.
    unsafe {
        if D3DKMTEnumAdapters2(&mut request) != 0 || !(1..=256).contains(&request.NumAdapters) {
            return result;
        }
        let mut entries = vec![D3DKMT_ADAPTERINFO::default(); request.NumAdapters as usize];
        request.pAdapters = entries.as_mut_ptr();
        if D3DKMTEnumAdapters2(&mut request) != 0 {
            return result;
        }
        for entry in entries.into_iter().take(request.NumAdapters as usize) {
            let adapter = Adapter(entry.hAdapter);
            let mut address = D3DKMT_ADAPTERADDRESS::default();
            let mut query = D3DKMT_QUERYADAPTERINFO {
                hAdapter: adapter.0,
                Type: KMTQAITYPE_ADAPTERADDRESS,
                pPrivateDriverData: (&mut address as *mut D3DKMT_ADAPTERADDRESS).cast(),
                PrivateDriverDataSize: size_of::<D3DKMT_ADAPTERADDRESS>() as u32,
            };
            if D3DKMTQueryAdapterInfo(&mut query) != 0
                || address.BusNumber > 255
                || address.DeviceNumber > 31
                || address.FunctionNumber > 7
            {
                continue;
            }
            let mut count = D3DKMT_PHYSICAL_ADAPTER_COUNT::default();
            query.Type = KMTQAITYPE_PHYSICALADAPTERCOUNT;
            query.pPrivateDriverData = (&mut count as *mut D3DKMT_PHYSICAL_ADAPTER_COUNT).cast();
            query.PrivateDriverDataSize = size_of::<D3DKMT_PHYSICAL_ADAPTER_COUNT>() as u32;
            if D3DKMTQueryAdapterInfo(&mut query) != 0 || count.Count != 1 {
                continue;
            }
            // shortcut: linked adapters and nonzero PCI domains stay unavailable;
            // add physical-adapter mapping when supported hardware can test it.
            let pci = PciAddress {
                domain: 0,
                bus: address.BusNumber as u8,
                device: address.DeviceNumber as u8,
                function: address.FunctionNumber as u8,
            };
            let id = format!(
                "0x{:08x}_0x{:08x}_phys_0",
                entry.AdapterLuid.HighPart as u32, entry.AdapterLuid.LowPart
            );
            result
                .entry(pci)
                .and_modify(|value| *value = None)
                .or_insert(Some(id));
        }
    }
    result
}

fn counter_values(counter: PDH_HCOUNTER) -> Vec<(String, f64)> {
    let mut bytes = 0;
    let mut count = 0;
    // SAFETY: the typed allocation supplies alignment, includes the names, and
    // stays alive while pointers returned by PDH are read. All lengths are bounded.
    unsafe {
        if PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut bytes,
            &mut count,
            ptr::null_mut(),
        ) != PDH_MORE_DATA
            || bytes == 0
            || bytes > 16 * 1024 * 1024
        {
            return Vec::new();
        }
        let mut buffer = vec![
            PDH_FMT_COUNTERVALUE_ITEM_W::default();
            (bytes as usize).div_ceil(size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>())
        ];
        let allocated = bytes as usize;
        if PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut bytes,
            &mut count,
            buffer.as_mut_ptr(),
        ) != 0
            || bytes as usize > allocated
            || count as usize * size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() > bytes as usize
        {
            return Vec::new();
        }
        let start = buffer.as_ptr() as usize;
        let end = start + bytes as usize;
        buffer
            .iter()
            .take(count as usize)
            .filter_map(|item| {
                if !matches!(
                    item.FmtValue.CStatus,
                    PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
                ) {
                    return None;
                }
                let name = item.szName as usize;
                if name < start || name >= end || !name.is_multiple_of(2) {
                    return None;
                }
                let name = std::slice::from_raw_parts(item.szName, (end - name) / 2);
                let length = name.iter().position(|c| *c == 0)?;
                let value = item.FmtValue.Anonymous.doubleValue;
                (value.is_finite() && value >= 0.0).then(|| {
                    (
                        String::from_utf16_lossy(&name[..length]).to_ascii_lowercase(),
                        value,
                    )
                })
            })
            .collect()
    }
}

fn metrics(id: &str, engines: &[(String, f64)], memory: &[(String, f64)]) -> Option<GpuTelemetry> {
    let mut per_engine = BTreeMap::<u32, f64>::new();
    for (name, value) in engines {
        let Some((_, instance)) = name.split_once("_luid_") else {
            continue;
        };
        let Some((adapter, engine)) = instance.split_once("_eng_") else {
            continue;
        };
        let Some(engine) = engine.split('_').next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        if adapter == id && value.is_finite() && *value >= 0.0 {
            *per_engine.entry(engine).or_default() += value;
        }
    }
    // Processes share an engine; independent engines can run simultaneously.
    let utilization = per_engine
        .into_values()
        .reduce(f64::max)
        .map(|v| v.min(100.0));
    let name = format!("luid_{id}");
    let memory = memory
        .iter()
        .find(|(n, v)| n == &name && v.is_finite() && *v >= 0.0)
        .map(|(_, v)| v / 1_048_576.0);
    (utilization.is_some() || memory.is_some()).then_some(GpuTelemetry {
        samples: 1,
        gpu_utilization_percent: utilization,
        vram_used_mib: memory,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_match_the_exact_adapter_and_sum_only_the_same_engine() {
        let id = "0x00000000_0x00000001_phys_0";
        let engine = |pid, gpu, number, value| {
            (
                format!(
                    "pid_{pid}_luid_0x00000000_0x{gpu:08x}_phys_0_eng_{number}_engtype_compute"
                ),
                value,
            )
        };
        let engines = vec![
            engine(1, 1, 0, 60.0),
            engine(2, 1, 0, 30.0),
            engine(1, 1, 1, 75.0),
            engine(1, 2, 0, 100.0),
            engine(3, 1, 0, f64::NAN),
        ];
        let memory = vec![(format!("luid_{id}"), 4.0 * 1_048_576.0)];
        let sample = metrics(id, &engines, &memory).unwrap();
        assert_eq!(sample.gpu_utilization_percent, Some(90.0));
        assert_eq!(sample.vram_used_mib, Some(4.0));
        assert_eq!(sample.power_watts, None);
        assert!(metrics("0x00000000_0x00000003_phys_0", &engines, &memory).is_none());
        assert!(metrics(id, &[], &[]).is_none());
        assert_eq!(
            metrics(id, &[engine(1, 1, 0, 105.0)], &[])
                .unwrap()
                .gpu_utilization_percent,
            Some(100.0)
        );
    }

    #[test]
    #[ignore = "reads real Windows WDDM counters; no GPU workload"]
    fn live_windows_counters_match_a_selected_pci_adapter() {
        let pci = std::env::var("PICKAXE_TELEMETRY_TEST_PCI")
            .expect("select the adapter under observation");
        let pci = PciAddress::parse(&pci).expect("valid PCI address");
        let sampler = Sampler::new().expect("Windows GPU counters available");
        assert!(
            sampler.adapters.contains_key(&pci),
            "available PCI adapters: {:?}",
            sampler.adapters.keys()
        );
        drop(sampler);
        let mut live = super::super::LiveTelemetrySampler::start(vec![Some(
            super::super::TelemetrySource::Windows(pci),
        )]);
        std::thread::sleep(std::time::Duration::from_secs(3));
        let sample = live.snapshot();
        live.stop();
        assert!(sample
            .gpu_utilization_percent
            .is_some_and(|v| (0.0..=100.0).contains(&v)));
        assert!(sample.vram_used_mib.is_some_and(|v| v >= 0.0));
        println!("PCI {pci}: {}", serde_json::to_string(&sample).unwrap());
    }
}
