# Third-party sources

## wgpu-hal 30.0.1 with gfx-rs/wgpu#10124

`wgpu-hal/` is the wgpu-hal 30.0.1 crate as published on crates.io (its
`.crate` SHA-256, `b6b7fb58561a792bc237628ba0792e332de418fefe145f13b5ed8201e6d52f58`,
matches the checksum Cargo recorded for it), plus one upstream change from
wgpu's trunk: [gfx-rs/wgpu#10124](https://github.com/gfx-rs/wgpu/pull/10124),
"Fix crash on older vulkan drivers when `poolSizeCount == 0`", in
`src/vulkan/descriptor.rs`. Cargo uses it through `[patch.crates-io]` in the
root `Cargo.toml`.

wgpu 30 creates an empty bind group layout, and with it a descriptor pool
without pool sizes, while creating every device. Vulkan allowed such a pool
only from 1.3.215; Intel's driver for HD 520 and UHD 630 (`igvk64.dll`)
crashes on it with an access violation
([gfx-rs/wgpu#9845](https://github.com/gfx-rs/wgpu/issues/9845), Pickaxe
issue #30).

Remove this directory and the `[patch.crates-io]` entry once a wgpu-hal
release contains #10124. It is licensed MIT OR Apache-2.0, like wgpu
(`wgpu-hal/LICENSE.MIT`, `wgpu-hal/LICENSE.APACHE`).
