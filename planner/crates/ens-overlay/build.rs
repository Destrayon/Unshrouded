//! Compiles shaders/meter.wgsl to SPIR-V for the layer (docs/plans/overlay-parity.md).

fn main() {
    println!("cargo:rerun-if-changed=shaders/meter.wgsl");
    let src = std::fs::read_to_string("shaders/meter.wgsl").expect("shaders/meter.wgsl");
    let module = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&src)));
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
    // the shader is written for Vulkan's y-down clip space: no WebGPU y flip
    let mut options = naga::back::spv::Options { lang_version: (1, 0), ..Default::default() };
    options.flags.remove(naga::back::spv::WriterFlags::ADJUST_COORDINATE_SPACE);
    let words = naga::back::spv::write_vec(&module, &info, &options, None).expect("SPIR-V");
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("meter.spv"), bytes).unwrap();
}
