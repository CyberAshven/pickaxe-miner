//! Offline integration gates for the pinned upstream engine; no mining or keys from disk.
use libloading::Library;
use secp256k1::{PublicKey, SecretKey};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::ffi::{c_char, c_void, CStr};
use std::time::Instant;

type Create = unsafe extern "C" fn(*mut *mut c_void) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);
type Pubkey = unsafe extern "C" fn(*mut c_void, *const u8, *mut u8) -> i32;
type Sign = unsafe extern "C" fn(*mut c_void, *const u8, *const u8, *const u8, *mut u8) -> i32;
type GpuCreate = unsafe extern "C" fn(*mut *mut c_void, u32, u32) -> i32;
type Multiply = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut u8) -> i32;

#[repr(C)]
struct DeviceInfo {
    name: [u8; 128],
    global_mem_bytes: u64,
    compute_units: u32,
    max_clock_mhz: u32,
    max_threads_per_block: u32,
    backend_id: u32,
    device_index: u32,
}

struct Engine {
    _library: Library,
    create: Create,
    destroy: Destroy,
    pubkey: Pubkey,
    sign: Sign,
    gpu_create: GpuCreate,
    gpu_destroy: Destroy,
    multiply: Multiply,
    device_count: unsafe extern "C" fn(u32) -> u32,
    device_info: unsafe extern "C" fn(u32, u32, *mut DeviceInfo) -> i32,
}

impl Engine {
    fn load() -> Self {
        let path = std::env::var_os("PICKAXE_UFSECP_LIBRARY")
            .expect("set PICKAXE_UFSECP_LIBRARY to the trusted, pinned upstream shared library");
        // SAFETY: this explicit offline test loads a locally built trusted library.
        // Signatures below match v4.6.0 ufsecp.h and ufsecp_gpu.h. The library stays
        // owned by Engine until every borrowed Context has been destroyed.
        unsafe {
            let library = Library::new(path).expect("load upstream shared library");
            let abi = library
                .get::<unsafe extern "C" fn() -> u32>(b"ufsecp_abi_version\0")
                .unwrap()();
            assert_eq!(abi, 4, "unsupported upstream ABI");
            let version = library
                .get::<unsafe extern "C" fn() -> *const c_char>(b"ufsecp_version_string\0")
                .unwrap()();
            assert!(!version.is_null());
            println!(
                "{}",
                json!({"engine": CStr::from_ptr(version).to_string_lossy(), "abi": abi})
            );
            Self {
                create: *library.get(b"ufsecp_ctx_create\0").unwrap(),
                destroy: *library.get(b"ufsecp_ctx_destroy\0").unwrap(),
                pubkey: *library.get(b"ufsecp_pubkey_create\0").unwrap(),
                sign: *library.get(b"ufsecp_schnorr_sign\0").unwrap(),
                gpu_create: *library.get(b"ufsecp_gpu_ctx_create\0").unwrap(),
                gpu_destroy: *library.get(b"ufsecp_gpu_ctx_destroy\0").unwrap(),
                multiply: *library.get(b"ufsecp_gpu_generator_mul_batch\0").unwrap(),
                device_count: *library.get(b"ufsecp_gpu_device_count\0").unwrap(),
                device_info: *library.get(b"ufsecp_gpu_device_info\0").unwrap(),
                _library: library,
            }
        }
    }
}

struct Context<'a> {
    ptr: *mut c_void,
    destroy: Destroy,
    _library: &'a Library,
}

impl<'a> Context<'a> {
    fn new(engine: &'a Engine, gpu: Option<(u32, u32)>) -> Self {
        let mut ptr = std::ptr::null_mut();
        // SAFETY: output pointer is writable; GPU indices come from discovery.
        let (rc, destroy) = unsafe {
            match gpu {
                Some((backend, device)) => (
                    (engine.gpu_create)(&mut ptr, backend, device),
                    engine.gpu_destroy,
                ),
                None => ((engine.create)(&mut ptr), engine.destroy),
            }
        };
        assert_eq!(rc, 0, "upstream context creation failed");
        assert!(!ptr.is_null());
        Self {
            ptr,
            destroy,
            _library: &engine._library,
        }
    }
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        // SAFETY: exactly one owner; Engine and its DLL still exist.
        unsafe { (self.destroy)(self.ptr) }
    }
}

#[test]
#[ignore = "requires the pinned upstream shared library; synthetic CPU keys only"]
fn cpu_signature_compatibility() {
    let engine = Engine::load();
    let ctx = Context::new(&engine, None);
    for index in 0u32..64 {
        let key: [u8; 32] = Sha256::digest(index.to_be_bytes()).into();
        let message: [u8; 32] = Sha256::digest(key).into();
        let secret = SecretKey::from_secret_bytes(key).unwrap();
        let public = PublicKey::from_secret_key(&secret);
        let mut upstream_public = [0; 33];
        let mut signature = [0; 64];
        // SAFETY: all input/output arrays match the pinned C ABI's fixed lengths.
        unsafe {
            assert_eq!(
                (engine.pubkey)(ctx.ptr, key.as_ptr(), upstream_public.as_mut_ptr()),
                0
            );
            assert_eq!(
                (engine.sign)(
                    ctx.ptr,
                    message.as_ptr(),
                    key.as_ptr(),
                    [0u8; 32].as_ptr(),
                    signature.as_mut_ptr()
                ),
                0
            );
        }
        assert_eq!(upstream_public, public.serialize());
        let bip340 = secp256k1::schnorr::Signature::from_byte_array(signature);
        assert!(
            secp256k1::schnorr::verify(&bip340, &message, &public.x_only_public_key().0).is_ok()
        );
        assert!(
            !crate::crypto::bch_schnorr_verify(&upstream_public, &message, &signature).unwrap()
        );
        let bch = crate::crypto::bch_schnorr_sign(&key, &message).unwrap();
        assert!(crate::crypto::bch_schnorr_verify(&upstream_public, &message, &bch).unwrap());
    }
    println!(
        "{}",
        json!({"vectors":64,"public_keys_match":true,"bip340_valid":true,"bip340_is_not_bch":true,"drop_in_photon_signer":false})
    );
}

#[test]
#[ignore = "requires the pinned BCH shim probe library; synthetic CPU keys only"]
fn bch_shim_signature_compatibility() {
    type Nonce =
        unsafe extern "C" fn(*mut u8, *const u8, *const u8, *const u8, *mut c_void, u32) -> i32;
    type BchSign = unsafe extern "C" fn(
        *const c_void,
        *mut u8,
        *const u8,
        *const u8,
        Option<Nonce>,
        *const c_void,
    ) -> i32;
    type Parse = unsafe extern "C" fn(*const c_void, *mut u8, *const u8, usize) -> i32;
    type Verify = unsafe extern "C" fn(*const c_void, *const u8, *const u8, *const u8) -> i32;
    let path = std::env::var_os("PICKAXE_BCHN_LIBRARY")
        .expect("set PICKAXE_BCHN_LIBRARY to the trusted pinned BCH shim library");
    // SAFETY: symbols and layouts match the pinned BCH shim headers. The opaque
    // pubkey is exactly unsigned char data[64]. Context cannot outlive the DLL.
    let (library, create, destroy, sign, parse, verify) = unsafe {
        let library = Library::new(path).unwrap();
        let create = *library
            .get::<unsafe extern "C" fn(u32) -> *mut c_void>(b"secp256k1_context_create\0")
            .unwrap();
        let destroy = *library
            .get::<Destroy>(b"secp256k1_context_destroy\0")
            .unwrap();
        let sign = *library.get::<BchSign>(b"secp256k1_schnorr_sign\0").unwrap();
        let parse = *library
            .get::<Parse>(b"secp256k1_ec_pubkey_parse\0")
            .unwrap();
        let verify = *library
            .get::<Verify>(b"secp256k1_schnorr_verify\0")
            .unwrap();
        (library, create, destroy, sign, parse, verify)
    };
    // SAFETY: context flags are SIGN | VERIFY from the pinned shim header.
    let ptr = unsafe { create(1 | (1 << 8) | (1 << 9)) };
    assert!(!ptr.is_null());
    let ctx = Context {
        ptr,
        destroy,
        _library: &library,
    };
    let mut different_signatures = Vec::new();
    for index in 0u32..67 {
        let key: [u8; 32] = Sha256::digest(index.to_be_bytes()).into();
        let message: [u8; 32] = match index {
            64 => [0; 32],
            65 => secp256k1::constants::CURVE_ORDER,
            66 => [255; 32],
            _ => Sha256::digest(key).into(),
        };
        let public = crate::crypto::compressed_pubkey(&key).unwrap();
        let expected = crate::crypto::bch_schnorr_sign(&key, &message).unwrap();
        let mut signature = [0; 64];
        let mut parsed = [0u8; 64];
        // SAFETY: each buffer matches its ABI size; pointers remain valid for
        // these synchronous calls, and default RFC6979 uses no callback data.
        unsafe {
            assert_eq!(
                sign(
                    ctx.ptr,
                    signature.as_mut_ptr(),
                    message.as_ptr(),
                    key.as_ptr(),
                    None,
                    std::ptr::null()
                ),
                1
            );
            assert_eq!(
                parse(ctx.ptr, parsed.as_mut_ptr(), public.as_ptr(), public.len()),
                1
            );
            assert_eq!(
                verify(
                    ctx.ptr,
                    signature.as_ptr(),
                    message.as_ptr(),
                    parsed.as_ptr()
                ),
                1
            );
            assert!(crate::crypto::bch_schnorr_verify(&public, &message, &signature).unwrap());
            if signature != expected {
                different_signatures.push(index);
            }
            assert_eq!(
                verify(
                    ctx.ptr,
                    expected.as_ptr(),
                    message.as_ptr(),
                    parsed.as_ptr()
                ),
                1
            );
            signature[32] ^= 1;
            assert_eq!(
                verify(
                    ctx.ptr,
                    signature.as_ptr(),
                    message.as_ptr(),
                    parsed.as_ptr()
                ),
                0
            );
        }
        assert!(crate::crypto::bch_schnorr_verify(&public, &message, &expected).unwrap());
    }
    // BCHN feeds the raw message into RFC6979. Pickaxe reduces it modulo n.
    // Both produce valid BCH signatures; only messages >= n differ here.
    assert_eq!(different_signatures, [65, 66]);
    for invalid in [[0; 32], secp256k1::constants::CURVE_ORDER, [255; 32]] {
        let mut signature = [0xaa; 64];
        // SAFETY: invalid scalar values still occupy valid 32-byte buffers.
        assert_eq!(
            unsafe {
                sign(
                    ctx.ptr,
                    signature.as_mut_ptr(),
                    [0u8; 32].as_ptr(),
                    invalid.as_ptr(),
                    None,
                    std::ptr::null(),
                )
            },
            0
        );
        assert_eq!(signature, [0; 64]);
    }
    println!(
        "{}",
        json!({"bch_vectors":67,"signatures_byte_identical":65,"boundary_nonce_differences":different_signatures,"all_signatures_cross_verified":true,"tampering_rejected":true,"invalid_keys_cleared":true})
    );
}

#[test]
#[ignore = "exclusive GPU test; stop the live miner first; no signing or broadcasts"]
fn gpu_generator_compatibility_and_throughput() {
    let engine = Engine::load();
    let mut tested_devices = 0;
    for backend in [1, 2] {
        // CUDA, OpenCL; Metal requires an Apple host.
        // SAFETY: discovery has no caller-owned buffer.
        let devices = unsafe { (engine.device_count)(backend) };
        println!("{}", json!({"backend":backend,"devices":devices}));
        for device in 0..devices {
            let mut info = std::mem::MaybeUninit::<DeviceInfo>::zeroed();
            // SAFETY: repr(C) matches the pinned header, with writable storage.
            let info = unsafe {
                assert_eq!((engine.device_info)(backend, device, info.as_mut_ptr()), 0);
                info.assume_init()
            };
            let end = info
                .name
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(info.name.len());
            println!(
                "{}",
                json!({"backend":backend,"device":device,"name":String::from_utf8_lossy(&info.name[..end]),"compute_units":info.compute_units,"memory_bytes":info.global_mem_bytes})
            );
            let ctx = Context::new(&engine, Some((backend, device)));
            tested_devices += 1;
            for count in [16usize, 4096, 65_536, 262_144] {
                let mut scalars = vec![0u8; count * 32];
                for (i, scalar) in scalars.as_chunks_mut::<32>().0.iter_mut().enumerate() {
                    scalar[24..].copy_from_slice(&(i as u64 + 1).to_be_bytes());
                    if count == 16 && i % 2 == 1 {
                        scalar.copy_from_slice(&Sha256::digest((i as u64).to_be_bytes()));
                    }
                }
                let mut points = vec![0u8; count * 33];
                let check_indices: Vec<_> = if count <= 4096 {
                    (0..count).collect()
                } else {
                    vec![0, 1, count / 2, count - 2, count - 1]
                };
                let mut seconds = Vec::new();
                for round in 0..6 {
                    let start = Instant::now();
                    // SAFETY: contiguous host buffers have exactly count elements.
                    // Both remain alive throughout the synchronous public C API call.
                    let rc = unsafe {
                        (engine.multiply)(ctx.ptr, scalars.as_ptr(), count, points.as_mut_ptr())
                    };
                    let elapsed = start.elapsed().as_secs_f64();
                    assert_eq!(
                        rc, 0,
                        "GPU operation failed: backend={backend}, device={device}"
                    );
                    for &i in &check_indices {
                        let secret = SecretKey::from_secret_bytes(
                            scalars[i * 32..(i + 1) * 32].try_into().unwrap(),
                        )
                        .unwrap();
                        assert_eq!(
                            points[i * 33..(i + 1) * 33],
                            PublicKey::from_secret_key(&secret).serialize()
                        );
                    }
                    if round != 0 {
                        seconds.push(elapsed);
                    }
                }
                seconds.sort_by(f64::total_cmp);
                println!(
                    "{}",
                    json!({"backend":backend,"device":device,"count":count,"median_seconds":seconds[2],"million_points_per_second":count as f64 / seconds[2] / 1e6,"includes_host_transfers":true,"samples_seconds":seconds,"full_photon_pipeline":false})
                );
                if seconds[2] > 0.1 {
                    println!(
                        "{}",
                        json!({"backend":backend,"device":device,"larger_batches_skipped":"avoid long dispatches on Windows display GPUs"})
                    );
                    break;
                }
            }
        }
    }
    assert!(tested_devices > 0, "no upstream GPU device was tested");
}
