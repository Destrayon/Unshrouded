//! The Vulkan implicit layer `VK_LAYER_ENS_meter` (docs/plans/game-overlay.md). The loader calls
//! `vkNegotiateLoaderLayerInterfaceVersion` and then reaches every call through our get-proc-address functions. In
//! any process but enshrouded.exe the layer only links itself into the chain and hands every other call straight to
//! the next layer. In the game it intercepts swapchain creation (to make a render pass, framebuffers and a blending
//! pipeline for its images) and `vkQueuePresentKHR`, where it blends the meter image (overlay.bin next to this DLL) onto
//! the frame at the place the frame names (docs/plans/overlay-parity.md): a submit on the presenting queue that waits
//! for the game's semaphores, uploads a changed meter image into its texture, draws it as one quad with alpha blending,
//! and signals the semaphore the present then waits on.
//!
//! Loader structures and constants are from the Vulkan loader's vk_layer.h (loader interface version 2).

// results of cleanup and command-recording calls are ignored on purpose: a failure there only skips a frame
#![allow(unused_must_use)]

use crate::draw;
use crate::input;
use crate::frame::{Frame, AT_CURSOR, FILE, STALE_MS};
use ash::vk::{self, Handle};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::{Arc, Mutex, OnceLock};

type Gipa = unsafe extern "system" fn(vk::Instance, *const c_char) -> vk::PFN_vkVoidFunction;
type Gdpa = unsafe extern "system" fn(vk::Device, *const c_char) -> vk::PFN_vkVoidFunction;
type SetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;

/// vk_layer.h: VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO = 47, _DEVICE_CREATE_INFO = 48; VkLayerFunction
/// VK_LAYER_LINK_INFO = 0, VK_LOADER_DATA_CALLBACK = 1; LAYER_NEGOTIATE_INTERFACE_STRUCT = 1.
const LOADER_INSTANCE_CREATE_INFO: i32 = 47;
const LOADER_DEVICE_CREATE_INFO: i32 = 48;
const LAYER_LINK_INFO: i32 = 0;
const LOADER_DATA_CALLBACK: i32 = 1;

#[repr(C)]
struct LayerInstanceLink {
    next: *mut LayerInstanceLink,
    next_gipa: Gipa,
    next_gpdpa: *const c_void,
}

/// VkLayerInstanceCreateInfo: its union is two pointers wide (the layerDevice member); we use its first pointer.
#[repr(C)]
struct LayerInstanceCreateInfo {
    s_type: i32,
    p_next: *const c_void,
    function: i32,
    u: *mut c_void,
    _u2: *mut c_void,
}

#[repr(C)]
struct LayerDeviceLink {
    next: *mut LayerDeviceLink,
    next_gipa: Gipa,
    next_gdpa: Gdpa,
}

#[repr(C)]
struct LayerDeviceCreateInfo {
    s_type: i32,
    p_next: *const c_void,
    function: i32,
    u: *mut c_void,
}

#[repr(C)]
pub struct NegotiateLayerInterface {
    s_type: i32,
    p_next: *mut c_void,
    loader_layer_interface_version: u32,
    gipa: Option<Gipa>,
    gdpa: Option<Gdpa>,
    gpdpa: *const c_void,
}

/// Every dispatchable object starts with the loader's dispatch table pointer, shared by an instance and its physical
/// devices, and by a device, its queues and command buffers: the key the layer's data is found by.
unsafe fn key<T: Handle>(h: T) -> usize {
    unsafe { *(h.as_raw() as *const usize) }
}

struct Inst {
    instance: vk::Instance,
    gipa: Gipa,
    destroy: vk::PFN_vkDestroyInstance,
    mem_props: vk::PFN_vkGetPhysicalDeviceMemoryProperties,
    qf_props: vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties,
    surf_caps: Option<vk::PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR>,
}

/// Per swapchain image: the command buffer, fence and semaphore of its draw, the staging buffer the frame's pixels go
/// through, and the meter texture with its descriptor set.
struct Slot {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    sem: vk::Semaphore,
    buf: vk::Buffer,
    mem: vk::DeviceMemory,
    ptr: *mut u8,
    size: u64,
    tex: Option<Tex>,
    /// The breakdown tooltip's texture.
    tip: Option<Tex>,
    used: bool,
}

struct Tex {
    image: vk::Image,
    mem: vk::DeviceMemory,
    view: vk::ImageView,
    set: vk::DescriptorSet,
    width: u32,
    height: u32,
    /// The frame sequence number uploaded into it (0: nothing yet, layout undefined).
    seq: u64,
}

struct Swap {
    /// The game's window (0: unknown).
    hwnd: isize,
    extent: vk::Extent2D,
    tex_format: vk::Format,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    render_pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    slots: Vec<Option<Slot>>,
}

/// What every swapchain's pipeline shares: the shader, the texture binding and a pool of descriptor sets.
struct Shared {
    shader: vk::ShaderModule,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pool: vk::DescriptorPool,
}

/// shaders/meter.wgsl, compiled to SPIR-V by build.rs.
const SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/meter.spv"));

macro_rules! fns {
    ($($field:ident: $ty:ty = $name:literal),* $(,)?) => {
        struct DevFns { $($field: $ty),* }
        impl DevFns {
            unsafe fn load(gdpa: Gdpa, device: vk::Device) -> Option<DevFns> {
                Some(DevFns { $($field: unsafe { std::mem::transmute::<unsafe extern "system" fn(), $ty>(gdpa(device, concat!($name, "\0").as_ptr() as *const c_char)?) }),* })
            }
        }
    };
}

fns! {
    destroy_device: vk::PFN_vkDestroyDevice = "vkDestroyDevice",
    get_device_queue: vk::PFN_vkGetDeviceQueue = "vkGetDeviceQueue",
    queue_present: vk::PFN_vkQueuePresentKHR = "vkQueuePresentKHR",
    create_swapchain: vk::PFN_vkCreateSwapchainKHR = "vkCreateSwapchainKHR",
    destroy_swapchain: vk::PFN_vkDestroySwapchainKHR = "vkDestroySwapchainKHR",
    get_swapchain_images: vk::PFN_vkGetSwapchainImagesKHR = "vkGetSwapchainImagesKHR",
    create_command_pool: vk::PFN_vkCreateCommandPool = "vkCreateCommandPool",
    destroy_command_pool: vk::PFN_vkDestroyCommandPool = "vkDestroyCommandPool",
    allocate_command_buffers: vk::PFN_vkAllocateCommandBuffers = "vkAllocateCommandBuffers",
    begin_command_buffer: vk::PFN_vkBeginCommandBuffer = "vkBeginCommandBuffer",
    end_command_buffer: vk::PFN_vkEndCommandBuffer = "vkEndCommandBuffer",
    reset_command_buffer: vk::PFN_vkResetCommandBuffer = "vkResetCommandBuffer",
    cmd_pipeline_barrier: vk::PFN_vkCmdPipelineBarrier = "vkCmdPipelineBarrier",
    cmd_copy_buffer_to_image: vk::PFN_vkCmdCopyBufferToImage = "vkCmdCopyBufferToImage",
    queue_submit: vk::PFN_vkQueueSubmit = "vkQueueSubmit",
    create_fence: vk::PFN_vkCreateFence = "vkCreateFence",
    destroy_fence: vk::PFN_vkDestroyFence = "vkDestroyFence",
    wait_for_fences: vk::PFN_vkWaitForFences = "vkWaitForFences",
    reset_fences: vk::PFN_vkResetFences = "vkResetFences",
    create_semaphore: vk::PFN_vkCreateSemaphore = "vkCreateSemaphore",
    destroy_semaphore: vk::PFN_vkDestroySemaphore = "vkDestroySemaphore",
    create_buffer: vk::PFN_vkCreateBuffer = "vkCreateBuffer",
    destroy_buffer: vk::PFN_vkDestroyBuffer = "vkDestroyBuffer",
    get_buffer_memory_requirements: vk::PFN_vkGetBufferMemoryRequirements = "vkGetBufferMemoryRequirements",
    allocate_memory: vk::PFN_vkAllocateMemory = "vkAllocateMemory",
    free_memory: vk::PFN_vkFreeMemory = "vkFreeMemory",
    bind_buffer_memory: vk::PFN_vkBindBufferMemory = "vkBindBufferMemory",
    map_memory: vk::PFN_vkMapMemory = "vkMapMemory",
    device_wait_idle: vk::PFN_vkDeviceWaitIdle = "vkDeviceWaitIdle",
    create_image: vk::PFN_vkCreateImage = "vkCreateImage",
    destroy_image: vk::PFN_vkDestroyImage = "vkDestroyImage",
    get_image_memory_requirements: vk::PFN_vkGetImageMemoryRequirements = "vkGetImageMemoryRequirements",
    bind_image_memory: vk::PFN_vkBindImageMemory = "vkBindImageMemory",
    create_image_view: vk::PFN_vkCreateImageView = "vkCreateImageView",
    destroy_image_view: vk::PFN_vkDestroyImageView = "vkDestroyImageView",
    create_framebuffer: vk::PFN_vkCreateFramebuffer = "vkCreateFramebuffer",
    destroy_framebuffer: vk::PFN_vkDestroyFramebuffer = "vkDestroyFramebuffer",
    create_render_pass: vk::PFN_vkCreateRenderPass = "vkCreateRenderPass",
    destroy_render_pass: vk::PFN_vkDestroyRenderPass = "vkDestroyRenderPass",
    create_shader_module: vk::PFN_vkCreateShaderModule = "vkCreateShaderModule",
    destroy_shader_module: vk::PFN_vkDestroyShaderModule = "vkDestroyShaderModule",
    create_pipeline_layout: vk::PFN_vkCreatePipelineLayout = "vkCreatePipelineLayout",
    destroy_pipeline_layout: vk::PFN_vkDestroyPipelineLayout = "vkDestroyPipelineLayout",
    create_graphics_pipelines: vk::PFN_vkCreateGraphicsPipelines = "vkCreateGraphicsPipelines",
    destroy_pipeline: vk::PFN_vkDestroyPipeline = "vkDestroyPipeline",
    create_descriptor_set_layout: vk::PFN_vkCreateDescriptorSetLayout = "vkCreateDescriptorSetLayout",
    destroy_descriptor_set_layout: vk::PFN_vkDestroyDescriptorSetLayout = "vkDestroyDescriptorSetLayout",
    create_descriptor_pool: vk::PFN_vkCreateDescriptorPool = "vkCreateDescriptorPool",
    destroy_descriptor_pool: vk::PFN_vkDestroyDescriptorPool = "vkDestroyDescriptorPool",
    allocate_descriptor_sets: vk::PFN_vkAllocateDescriptorSets = "vkAllocateDescriptorSets",
    free_descriptor_sets: vk::PFN_vkFreeDescriptorSets = "vkFreeDescriptorSets",
    update_descriptor_sets: vk::PFN_vkUpdateDescriptorSets = "vkUpdateDescriptorSets",
    cmd_begin_render_pass: vk::PFN_vkCmdBeginRenderPass = "vkCmdBeginRenderPass",
    cmd_end_render_pass: vk::PFN_vkCmdEndRenderPass = "vkCmdEndRenderPass",
    cmd_bind_pipeline: vk::PFN_vkCmdBindPipeline = "vkCmdBindPipeline",
    cmd_bind_descriptor_sets: vk::PFN_vkCmdBindDescriptorSets = "vkCmdBindDescriptorSets",
    cmd_set_viewport: vk::PFN_vkCmdSetViewport = "vkCmdSetViewport",
    cmd_set_scissor: vk::PFN_vkCmdSetScissor = "vkCmdSetScissor",
    cmd_draw: vk::PFN_vkCmdDraw = "vkCmdDraw",
}

struct Dev {
    device: vk::Device,
    gdpa: Gdpa,
    f: DevFns,
    set_loader_data: Option<SetDeviceLoaderData>,
    get_device_queue2: Option<vk::PFN_vkGetDeviceQueue2>,
    physical: vk::PhysicalDevice,
    inst: Arc<Inst>,
    families: Vec<vk::QueueFamilyProperties>,
    mem: vk::PhysicalDeviceMemoryProperties,
    queues: HashMap<u64, u32>,
    pools: HashMap<u32, vk::CommandPool>,
    swaps: HashMap<u64, Swap>,
    shared: Option<Shared>,
}

unsafe impl Send for Dev {}

static INSTANCES: OnceLock<Mutex<HashMap<usize, Arc<Inst>>>> = OnceLock::new();
static DEVICES: OnceLock<Mutex<HashMap<usize, Arc<Mutex<Dev>>>>> = OnceLock::new();
static FRAME: Mutex<Option<Arc<Frame>>> = Mutex::new(None);
/// Each Win32 surface's window, for the swapchains made on it (input.rs watches that window).
static SURFACES: OnceLock<Mutex<HashMap<u64, isize>>> = OnceLock::new();

fn surfaces() -> &'static Mutex<HashMap<u64, isize>> {
    SURFACES.get_or_init(Default::default)
}

fn instances() -> &'static Mutex<HashMap<usize, Arc<Inst>>> {
    INSTANCES.get_or_init(Default::default)
}
fn devices() -> &'static Mutex<HashMap<usize, Arc<Mutex<Dev>>>> {
    DEVICES.get_or_init(Default::default)
}

/// Draw only inside the game (or anywhere with ENS_OVERLAY_ANY=1, for the test window).
fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("ENS_OVERLAY_ANY").is_ok_and(|v| v == "1")
            || std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase())).is_some_and(|n| n == "enshrouded.exe")
    })
}

/// The folder this DLL was loaded from, where overlay.bin is written.
fn own_dir() -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT};
    let mut module = std::ptr::null_mut();
    let probe = own_dir as *const u16;
    if unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, probe, &mut module) } == 0 {
        return None;
    }
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    path.parent().map(|p| p.to_path_buf())
}

/// Re-read overlay.bin whenever it changes (a background thread, so a present never waits on the disk).
fn start_reader() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        // ENS_OVERLAY_FRAME: another frame file, for the test window
        let path = std::env::var_os("ENS_OVERLAY_FRAME").map(std::path::PathBuf::from).or_else(|| own_dir().map(|d| d.join(FILE)));
        let Some(path) = path else { return };
        std::thread::spawn(move || {
            let mut seen = None;
            loop {
                let modified = std::fs::metadata(&path).and_then(|m| m.modified());
                // the planner removes the file to hide the meter (switch off, or not in a world)
                if modified.as_ref().is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) && seen.is_some() {
                    *FRAME.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    seen = None;
                }
                if let Ok(m) = modified {
                    if seen != Some(m) {
                        if let Some(f) = std::fs::read(&path).ok().and_then(|b| Frame::decode(&b)) {
                            *FRAME.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
                            seen = Some(m);
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
        });
    });
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn name_of(p: *const c_char) -> &'static [u8] {
    if p.is_null() { b"" } else { unsafe { CStr::from_ptr(p) }.to_bytes() }
}

macro_rules! pfn {
    ($f:expr, $ty:ty) => {
        Some(unsafe { std::mem::transmute::<$ty, unsafe extern "system" fn()>($f) })
    };
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(p: *mut NegotiateLayerInterface) -> vk::Result {
    let Some(p) = (unsafe { p.as_mut() }) else { return vk::Result::ERROR_INITIALIZATION_FAILED };
    if p.s_type != 1 || p.loader_layer_interface_version < 2 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    p.loader_layer_interface_version = 2;
    p.gipa = Some(get_instance_proc_addr);
    p.gdpa = Some(get_device_proc_addr);
    p.gpdpa = std::ptr::null();
    vk::Result::SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn get_instance_proc_addr(instance: vk::Instance, name: *const c_char) -> vk::PFN_vkVoidFunction {
    match name_of(name) {
        b"vkGetInstanceProcAddr" => return pfn!(get_instance_proc_addr, Gipa),
        b"vkGetDeviceProcAddr" => return pfn!(get_device_proc_addr, Gdpa),
        b"vkCreateInstance" => return pfn!(create_instance, vk::PFN_vkCreateInstance),
        b"vkDestroyInstance" => return pfn!(destroy_instance, vk::PFN_vkDestroyInstance),
        b"vkCreateDevice" => return pfn!(create_device, vk::PFN_vkCreateDevice),
        b"vkCreateWin32SurfaceKHR" if enabled() => return pfn!(create_win32_surface, vk::PFN_vkCreateWin32SurfaceKHR),
        n => {
            if enabled() {
                if let Some(f) = device_hook(n) {
                    return f;
                }
            }
        }
    }
    if instance == vk::Instance::null() {
        return None;
    }
    let inst = instances().lock().ok()?.get(&unsafe { key(instance) }).cloned()?;
    unsafe { (inst.gipa)(instance, name) }
}

/// The device calls the layer takes over in the game.
fn device_hook(n: &[u8]) -> Option<vk::PFN_vkVoidFunction> {
    Some(match n {
        b"vkDestroyDevice" => pfn!(destroy_device, vk::PFN_vkDestroyDevice),
        b"vkGetDeviceQueue" => pfn!(get_device_queue, vk::PFN_vkGetDeviceQueue),
        b"vkGetDeviceQueue2" => pfn!(get_device_queue2, vk::PFN_vkGetDeviceQueue2),
        b"vkQueuePresentKHR" => pfn!(queue_present, vk::PFN_vkQueuePresentKHR),
        b"vkCreateSwapchainKHR" => pfn!(create_swapchain, vk::PFN_vkCreateSwapchainKHR),
        b"vkDestroySwapchainKHR" => pfn!(destroy_swapchain, vk::PFN_vkDestroySwapchainKHR),
        _ => return None,
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn get_device_proc_addr(device: vk::Device, name: *const c_char) -> vk::PFN_vkVoidFunction {
    let n = name_of(name);
    if n == b"vkGetDeviceProcAddr" {
        return pfn!(get_device_proc_addr, Gdpa);
    }
    if n == b"vkDestroyDevice" || enabled() {
        if let Some(f) = device_hook(n) {
            return f;
        }
    }
    let d = devices().lock().ok()?.get(&unsafe { key(device) }).cloned()?;
    let gdpa = d.lock().ok()?.gdpa;
    unsafe { gdpa(device, name) }
}

unsafe fn find_chain<T>(mut p: *const c_void, s_type: i32, function: i32) -> *mut T {
    while !p.is_null() {
        let hdr = p as *const LayerDeviceCreateInfo;
        if unsafe { (*hdr).s_type == s_type && (*hdr).function == function } {
            return p as *mut T;
        }
        p = unsafe { (*hdr).p_next };
    }
    std::ptr::null_mut()
}

unsafe extern "system" fn create_instance(ci: *const vk::InstanceCreateInfo<'_>, alloc: *const vk::AllocationCallbacks<'_>, out: *mut vk::Instance) -> vk::Result {
    let chain: *mut LayerInstanceCreateInfo = unsafe { find_chain((*ci).p_next, LOADER_INSTANCE_CREATE_INFO, LAYER_LINK_INFO) };
    if chain.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let link = unsafe { (*chain).u as *mut LayerInstanceLink };
    let gipa = unsafe { (*link).next_gipa };
    unsafe { (*chain).u = (*link).next as *mut c_void };
    let Some(create) = (unsafe { gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()) }) else { return vk::Result::ERROR_INITIALIZATION_FAILED };
    let create: vk::PFN_vkCreateInstance = unsafe { std::mem::transmute(create) };
    let r = unsafe { create(ci, alloc, out) };
    if r != vk::Result::SUCCESS {
        return r;
    }
    let instance = unsafe { *out };
    let load = |n: &CStr| unsafe { gipa(instance, n.as_ptr()) };
    let (Some(destroy), Some(mem), Some(qf)) = (load(c"vkDestroyInstance"), load(c"vkGetPhysicalDeviceMemoryProperties"), load(c"vkGetPhysicalDeviceQueueFamilyProperties")) else { return r };
    let inst = Inst {
        instance,
        gipa,
        destroy: unsafe { std::mem::transmute(destroy) },
        mem_props: unsafe { std::mem::transmute(mem) },
        qf_props: unsafe { std::mem::transmute(qf) },
        surf_caps: load(c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR").map(|f| unsafe { std::mem::transmute(f) }),
    };
    if let Ok(mut m) = instances().lock() {
        m.insert(unsafe { key(instance) }, Arc::new(inst));
    }
    r
}

unsafe extern "system" fn create_win32_surface(instance: vk::Instance, ci: *const vk::Win32SurfaceCreateInfoKHR<'_>, alloc: *const vk::AllocationCallbacks<'_>, out: *mut vk::SurfaceKHR) -> vk::Result {
    let Some(inst) = instances().lock().ok().and_then(|m| m.get(&unsafe { key(instance) }).cloned()) else { return vk::Result::ERROR_INITIALIZATION_FAILED };
    let Some(next) = (unsafe { (inst.gipa)(instance, c"vkCreateWin32SurfaceKHR".as_ptr()) }) else { return vk::Result::ERROR_EXTENSION_NOT_PRESENT };
    let next: vk::PFN_vkCreateWin32SurfaceKHR = unsafe { std::mem::transmute(next) };
    let r = unsafe { next(instance, ci, alloc, out) };
    if r == vk::Result::SUCCESS {
        if let Ok(mut m) = surfaces().lock() {
            m.insert(unsafe { *out }.as_raw(), unsafe { (*ci).hwnd });
        }
    }
    r
}

unsafe extern "system" fn destroy_instance(instance: vk::Instance, alloc: *const vk::AllocationCallbacks<'_>) {
    let inst = instances().lock().ok().and_then(|mut m| m.remove(&unsafe { key(instance) }));
    if let Some(i) = inst {
        unsafe { (i.destroy)(instance, alloc) };
    }
}

unsafe extern "system" fn create_device(physical: vk::PhysicalDevice, ci: *const vk::DeviceCreateInfo<'_>, alloc: *const vk::AllocationCallbacks<'_>, out: *mut vk::Device) -> vk::Result {
    let chain: *mut LayerDeviceCreateInfo = unsafe { find_chain((*ci).p_next, LOADER_DEVICE_CREATE_INFO, LAYER_LINK_INFO) };
    let Some(inst) = instances().lock().ok().and_then(|m| m.get(&unsafe { key(physical) }).cloned()) else { return vk::Result::ERROR_INITIALIZATION_FAILED };
    if chain.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let link = unsafe { (*chain).u as *mut LayerDeviceLink };
    let (gipa, gdpa) = unsafe { ((*link).next_gipa, (*link).next_gdpa) };
    unsafe { (*chain).u = (*link).next as *mut c_void };
    let Some(create) = (unsafe { gipa(inst.instance, c"vkCreateDevice".as_ptr()) }) else { return vk::Result::ERROR_INITIALIZATION_FAILED };
    let create: vk::PFN_vkCreateDevice = unsafe { std::mem::transmute(create) };
    let r = unsafe { create(physical, ci, alloc, out) };
    if r != vk::Result::SUCCESS {
        return r;
    }
    let device = unsafe { *out };
    let cb: *mut LayerDeviceCreateInfo = unsafe { find_chain((*ci).p_next, LOADER_DEVICE_CREATE_INFO, LOADER_DATA_CALLBACK) };
    let set_loader_data: Option<SetDeviceLoaderData> = (!cb.is_null()).then(|| unsafe { std::mem::transmute((*cb).u) });
    let Some(f) = (unsafe { DevFns::load(gdpa, device) }) else {
        // a device without the swapchain calls: track it only so its proc addresses still resolve
        return r;
    };
    let mut mem = vk::PhysicalDeviceMemoryProperties::default();
    unsafe { (inst.mem_props)(physical, &mut mem) };
    let mut n = 0u32;
    unsafe { (inst.qf_props)(physical, &mut n, std::ptr::null_mut()) };
    let mut families = vec![vk::QueueFamilyProperties::default(); n as usize];
    unsafe { (inst.qf_props)(physical, &mut n, families.as_mut_ptr()) };
    let get_device_queue2 = unsafe { gdpa(device, c"vkGetDeviceQueue2".as_ptr()) }.map(|f| unsafe { std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkGetDeviceQueue2>(f) });
    let d = Dev { device, gdpa, f, set_loader_data, get_device_queue2, physical, inst, families, mem, queues: HashMap::new(), pools: HashMap::new(), swaps: HashMap::new(), shared: None };
    if let Ok(mut m) = devices().lock() {
        m.insert(unsafe { key(device) }, Arc::new(Mutex::new(d)));
    }
    if enabled() {
        start_reader();
    }
    r
}

unsafe extern "system" fn destroy_device(device: vk::Device, alloc: *const vk::AllocationCallbacks<'_>) {
    let Some(d) = devices().lock().ok().and_then(|mut m| m.remove(&unsafe { key(device) })) else { return };
    let mut d = d.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { (d.f.device_wait_idle)(device) };
    let swaps: Vec<u64> = d.swaps.keys().copied().collect();
    for s in swaps {
        unsafe { free_swap(&mut d, s) };
    }
    let pools: Vec<vk::CommandPool> = d.pools.drain().map(|(_, p)| p).collect();
    for pool in pools {
        unsafe { (d.f.destroy_command_pool)(device, pool, std::ptr::null()) };
    }
    unsafe { free_shared(&mut d) };
    unsafe { (d.f.destroy_device)(device, alloc) };
}

unsafe extern "system" fn get_device_queue(device: vk::Device, family: u32, index: u32, out: *mut vk::Queue) {
    let Some(d) = devices().lock().ok().and_then(|m| m.get(&unsafe { key(device) }).cloned()) else { return };
    let mut d = d.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { (d.f.get_device_queue)(device, family, index, out) };
    let q = unsafe { *out };
    d.queues.insert(q.as_raw(), family);
}

unsafe extern "system" fn get_device_queue2(device: vk::Device, info: *const vk::DeviceQueueInfo2<'_>, out: *mut vk::Queue) {
    let Some(d) = devices().lock().ok().and_then(|m| m.get(&unsafe { key(device) }).cloned()) else { return };
    let mut d = d.lock().unwrap_or_else(|e| e.into_inner());
    let Some(next) = d.get_device_queue2 else { return };
    unsafe { next(device, info, out) };
    let q = unsafe { *out };
    if q != vk::Queue::null() {
        d.queues.insert(q.as_raw(), unsafe { (*info).queue_family_index });
    }
}

unsafe extern "system" fn create_swapchain(device: vk::Device, ci: *const vk::SwapchainCreateInfoKHR<'_>, alloc: *const vk::AllocationCallbacks<'_>, out: *mut vk::SwapchainKHR) -> vk::Result {
    let Some(d) = devices().lock().ok().and_then(|m| m.get(&unsafe { key(device) }).cloned()) else { return vk::Result::ERROR_INITIALIZATION_FAILED };
    let mut d = d.lock().unwrap_or_else(|e| e.into_inner());
    let mut info = unsafe { *ci };
    let tex_format = draw::texture_format(info.image_format, info.image_color_space);
    // the meter is drawn in a render pass on the images: they need COLOR_ATTACHMENT, added when the surface allows it
    if tex_format.is_some() && !info.image_usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT) {
        if let Some(caps) = d.inst.surf_caps {
            let mut c = vk::SurfaceCapabilitiesKHR::default();
            if unsafe { caps(d.physical, info.surface, &mut c) } == vk::Result::SUCCESS && c.supported_usage_flags.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT) {
                info.image_usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT;
            }
        }
    }
    let r = unsafe { (d.f.create_swapchain)(device, &info, alloc, out) };
    let Some(tex_format) = tex_format.filter(|_| r == vk::Result::SUCCESS && info.image_usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)) else { return r };
    let sc = unsafe { *out };
    let mut n = 0u32;
    unsafe { (d.f.get_swapchain_images)(device, sc, &mut n, std::ptr::null_mut()) };
    let mut images = vec![vk::Image::null(); n as usize];
    unsafe { (d.f.get_swapchain_images)(device, sc, &mut n, images.as_mut_ptr()) };
    if let Some(mut s) = unsafe { new_swap(&mut d, info.image_format, info.image_extent, tex_format, images) } {
        s.hwnd = surfaces().lock().ok().and_then(|m| m.get(&info.surface.as_raw()).copied()).unwrap_or(0);
        d.swaps.insert(sc.as_raw(), s);
    }
    r
}

/// The shader, binding layout and descriptor pool, made on the first swapchain.
unsafe fn shared(d: &mut Dev) -> Option<&Shared> {
    if d.shared.is_none() {
        let f = &d.f;
        let code: Vec<u32> = SPIRV.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let mut shader = vk::ShaderModule::null();
        let mut set_layout = vk::DescriptorSetLayout::null();
        let mut layout = vk::PipelineLayout::null();
        let mut pool = vk::DescriptorPool::null();
        let binding = [vk::DescriptorSetLayoutBinding::default().binding(0).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).descriptor_count(1).stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)];
        let sizes = [vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLED_IMAGE, descriptor_count: 64 }];
        unsafe {
            if (f.create_shader_module)(d.device, &vk::ShaderModuleCreateInfo::default().code(&code), std::ptr::null(), &mut shader) != vk::Result::SUCCESS {
                return None;
            }
            (f.create_descriptor_set_layout)(d.device, &vk::DescriptorSetLayoutCreateInfo::default().bindings(&binding), std::ptr::null(), &mut set_layout);
            let sets = [set_layout];
            (f.create_pipeline_layout)(d.device, &vk::PipelineLayoutCreateInfo::default().set_layouts(&sets), std::ptr::null(), &mut layout);
            (f.create_descriptor_pool)(d.device, &vk::DescriptorPoolCreateInfo::default().flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET).max_sets(64).pool_sizes(&sizes), std::ptr::null(), &mut pool);
        }
        if set_layout == vk::DescriptorSetLayout::null() || layout == vk::PipelineLayout::null() || pool == vk::DescriptorPool::null() {
            return None;
        }
        d.shared = Some(Shared { shader, set_layout, layout, pool });
    }
    d.shared.as_ref()
}

/// Render pass, image views, framebuffers and blending pipeline for a new swapchain.
unsafe fn new_swap(d: &mut Dev, format: vk::Format, extent: vk::Extent2D, tex_format: vk::Format, images: Vec<vk::Image>) -> Option<Swap> {
    let (shader, layout) = unsafe { shared(d).map(|s| (s.shader, s.layout))? };
    let f = &d.f;
    let mut s = Swap { hwnd: 0, extent, tex_format, images, views: vec![], framebuffers: vec![], render_pass: vk::RenderPass::null(), pipeline: vk::Pipeline::null(), slots: vec![] };
    s.slots = (0..s.images.len()).map(|_| None).collect();
    unsafe {
        // keep what the game drew, draw on top, hand the image back ready to present
        let attach = [vk::AttachmentDescription::default().format(format).samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD).store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE).stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR).final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
        let color = [vk::AttachmentReference { attachment: 0, layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL }];
        let subpass = [vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS).color_attachments(&color)];
        let dep = [vk::SubpassDependency::default().src_subpass(vk::SUBPASS_EXTERNAL).dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT).dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE)];
        let rp = vk::RenderPassCreateInfo::default().attachments(&attach).subpasses(&subpass).dependencies(&dep);
        if (f.create_render_pass)(d.device, &rp, std::ptr::null(), &mut s.render_pass) != vk::Result::SUCCESS {
            return None;
        }
        let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
        for &image in &s.images {
            let mut view = vk::ImageView::null();
            let vi = vk::ImageViewCreateInfo::default().image(image).view_type(vk::ImageViewType::TYPE_2D).format(format).subresource_range(range);
            (f.create_image_view)(d.device, &vi, std::ptr::null(), &mut view);
            let views = [view];
            let mut fb = vk::Framebuffer::null();
            let fi = vk::FramebufferCreateInfo::default().render_pass(s.render_pass).attachments(&views).width(extent.width).height(extent.height).layers(1);
            (f.create_framebuffer)(d.device, &fi, std::ptr::null(), &mut fb);
            s.views.push(view);
            s.framebuffers.push(fb);
        }
        let stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(shader).name(c"vs"),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(shader).name(c"fs"),
        ];
        let vertex = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
        let viewport = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default().polygon_mode(vk::PolygonMode::FILL).cull_mode(vk::CullModeFlags::NONE).line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // straight alpha over the game's picture; the image's own alpha is left as it was
        let blend = [vk::PipelineColorBlendAttachmentState::default().blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA).dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA).color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO).dst_alpha_blend_factor(vk::BlendFactor::ONE).alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::R | vk::ColorComponentFlags::G | vk::ColorComponentFlags::B)];
        let blending = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend);
        let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
        let pi = [vk::GraphicsPipelineCreateInfo::default().stages(&stages).vertex_input_state(&vertex).input_assembly_state(&assembly)
            .viewport_state(&viewport).rasterization_state(&raster).multisample_state(&multisample).color_blend_state(&blending)
            .dynamic_state(&dynamic).layout(layout).render_pass(s.render_pass).subpass(0)];
        if (f.create_graphics_pipelines)(d.device, vk::PipelineCache::null(), 1, pi.as_ptr(), std::ptr::null(), &mut s.pipeline) != vk::Result::SUCCESS {
            free_swap_parts(d, s);
            return None;
        }
    }
    Some(s)
}

unsafe fn free_slot(d: &Dev, slot: Slot) {
    let f = &d.f;
    unsafe {
        if slot.used {
            (f.wait_for_fences)(d.device, 1, &slot.fence, vk::TRUE, 1_000_000_000);
        }
        (f.destroy_fence)(d.device, slot.fence, std::ptr::null());
        (f.destroy_semaphore)(d.device, slot.sem, std::ptr::null());
        (f.destroy_buffer)(d.device, slot.buf, std::ptr::null());
        (f.free_memory)(d.device, slot.mem, std::ptr::null());
        if let Some(t) = slot.tex {
            free_tex(d, t);
        }
        if let Some(t) = slot.tip {
            free_tex(d, t);
        }
    }
}

unsafe fn free_tex(d: &Dev, t: Tex) {
    let f = &d.f;
    unsafe {
        if let Some(s) = &d.shared {
            (f.free_descriptor_sets)(d.device, s.pool, 1, &t.set);
        }
        (f.destroy_image_view)(d.device, t.view, std::ptr::null());
        (f.destroy_image)(d.device, t.image, std::ptr::null());
        (f.free_memory)(d.device, t.mem, std::ptr::null());
    }
}

unsafe fn free_swap_parts(d: &Dev, s: Swap) {
    let f = &d.f;
    unsafe {
        for slot in s.slots.into_iter().flatten() {
            free_slot(d, slot);
        }
        for fb in s.framebuffers {
            (f.destroy_framebuffer)(d.device, fb, std::ptr::null());
        }
        for v in s.views {
            (f.destroy_image_view)(d.device, v, std::ptr::null());
        }
        if s.pipeline != vk::Pipeline::null() {
            (f.destroy_pipeline)(d.device, s.pipeline, std::ptr::null());
        }
        (f.destroy_render_pass)(d.device, s.render_pass, std::ptr::null());
    }
}

unsafe fn free_swap(d: &mut Dev, sc: u64) {
    if let Some(s) = d.swaps.remove(&sc) {
        unsafe { free_swap_parts(d, s) };
    }
}

unsafe fn free_shared(d: &mut Dev) {
    if let Some(s) = d.shared.take() {
        let f = &d.f;
        unsafe {
            (f.destroy_descriptor_pool)(d.device, s.pool, std::ptr::null());
            (f.destroy_pipeline_layout)(d.device, s.layout, std::ptr::null());
            (f.destroy_descriptor_set_layout)(d.device, s.set_layout, std::ptr::null());
            (f.destroy_shader_module)(d.device, s.shader, std::ptr::null());
        }
    }
}

unsafe extern "system" fn destroy_swapchain(device: vk::Device, sc: vk::SwapchainKHR, alloc: *const vk::AllocationCallbacks<'_>) {
    let Some(d) = devices().lock().ok().and_then(|m| m.get(&unsafe { key(device) }).cloned()) else { return };
    let mut d = d.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { free_swap(&mut d, sc.as_raw()) };
    unsafe { (d.f.destroy_swapchain)(device, sc, alloc) };
}

unsafe extern "system" fn queue_present(queue: vk::Queue, info: *const vk::PresentInfoKHR<'_>) -> vk::Result {
    let Some(d) = devices().lock().ok().and_then(|m| m.get(&unsafe { key(queue) }).cloned()) else { return vk::Result::ERROR_DEVICE_LOST };
    let mut d = d.lock().unwrap_or_else(|e| e.into_inner());
    let present = d.f.queue_present;
    if unsafe { (*info).swapchain_count } > 0 {
        let sc = unsafe { *(*info).p_swapchains }.as_raw();
        if let Some(s) = d.swaps.get(&sc) {
            input::poll(s.hwnd);
        }
    }
    if let Some(sem) = unsafe { draw_meter(&mut d, queue, &*info) } {
        let mut patched = unsafe { *info };
        patched.wait_semaphore_count = 1;
        patched.p_wait_semaphores = &sem;
        drop(d);
        return unsafe { present(queue, &patched) };
    }
    drop(d);
    unsafe { present(queue, info) }
}

/// A device-local texture of the meter's size with its view and descriptor set.
unsafe fn new_tex(d: &Dev, format: vk::Format, width: u32, height: u32) -> Option<Tex> {
    let f = &d.f;
    let s = d.shared.as_ref()?;
    unsafe {
        let ii = vk::ImageCreateInfo::default().image_type(vk::ImageType::TYPE_2D).format(format)
            .extent(vk::Extent3D { width, height, depth: 1 }).mip_levels(1).array_layers(1).samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL).usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE).initial_layout(vk::ImageLayout::UNDEFINED);
        let mut image = vk::Image::null();
        if (f.create_image)(d.device, &ii, std::ptr::null(), &mut image) != vk::Result::SUCCESS {
            return None;
        }
        let mut req = vk::MemoryRequirements::default();
        (f.get_image_memory_requirements)(d.device, image, &mut req);
        let ty = (0..d.mem.memory_type_count)
            .find(|&i| req.memory_type_bits & (1 << i) != 0 && d.mem.memory_types[i as usize].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL))
            .or_else(|| (0..d.mem.memory_type_count).find(|&i| req.memory_type_bits & (1 << i) != 0))?;
        let mut mem = vk::DeviceMemory::null();
        if (f.allocate_memory)(d.device, &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), std::ptr::null(), &mut mem) != vk::Result::SUCCESS {
            (f.destroy_image)(d.device, image, std::ptr::null());
            return None;
        }
        (f.bind_image_memory)(d.device, image, mem, 0);
        let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
        let mut view = vk::ImageView::null();
        (f.create_image_view)(d.device, &vk::ImageViewCreateInfo::default().image(image).view_type(vk::ImageViewType::TYPE_2D).format(format).subresource_range(range), std::ptr::null(), &mut view);
        let layouts = [s.set_layout];
        let mut set = vk::DescriptorSet::null();
        if (f.allocate_descriptor_sets)(d.device, &vk::DescriptorSetAllocateInfo::default().descriptor_pool(s.pool).set_layouts(&layouts), &mut set) != vk::Result::SUCCESS {
            (f.destroy_image_view)(d.device, view, std::ptr::null());
            (f.destroy_image)(d.device, image, std::ptr::null());
            (f.free_memory)(d.device, mem, std::ptr::null());
            return None;
        }
        let ii = [vk::DescriptorImageInfo { sampler: vk::Sampler::null(), image_view: view, image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL }];
        let w = [vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0).descriptor_type(vk::DescriptorType::SAMPLED_IMAGE).image_info(&ii)];
        (f.update_descriptor_sets)(d.device, 1, w.as_ptr(), 0, std::ptr::null());
        Some(Tex { image, mem, view, set, width, height, seq: 0 })
    }
}

/// Blend the meter onto the first swapchain's image of this present; returns the semaphore the present must wait on
/// instead of the game's, or None when nothing was drawn (no fresh frame, unsupported swapchain, GPU still busy).
unsafe fn draw_meter(d: &mut Dev, queue: vk::Queue, info: &vk::PresentInfoKHR<'_>) -> Option<vk::Semaphore> {
    let frame = FRAME.lock().ok()?.clone()?;
    if now_ms().saturating_sub(frame.unix_ms) > STALE_MS || info.swapchain_count == 0 {
        return None;
    }
    let family = *d.queues.get(&queue.as_raw())?;
    if !d.families.get(family as usize)?.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
        return None;
    }
    let sc = unsafe { *info.p_swapchains }.as_raw();
    let index = unsafe { *info.p_image_indices } as usize;
    let (extent, tex_format, render_pass, pipeline, framebuffer) = {
        let s = d.swaps.get(&sc)?;
        (s.extent, s.tex_format, s.render_pass, s.pipeline, *s.framebuffers.get(index)?)
    };
    // being resized: the current picture stretched to the new size until the planner's re-render arrives
    let (dw, dh) = input::size().map_or((frame.width, frame.height), |(w, h)| (w.min(extent.width), h.min(extent.height)));
    let mut offset = draw::place(dw, dh, extent, frame.place)?;
    // being dragged, or dragged and the planner has not caught up yet
    if let Some((x, y)) = input::position() {
        offset.x = x.clamp(0, extent.width as i32 - dw as i32);
        offset.y = y.clamp(0, extent.height as i32 - dh as i32);
    }
    // the breakdown tooltip beside the pointer, while the pointer is over the meter
    let tip = frame.extras.iter().find(|e| e.kind == AT_CURSOR && e.width <= extent.width && e.height <= extent.height)
        .zip(input::cursor()).map(|(e, (cx, cy))| (e, draw::beside(cx, cy, e.width, e.height, extent)));
    let main_bytes = frame.width as u64 * frame.height as u64 * 4;
    let bytes = main_bytes + tip.map_or(0, |(e, _)| e.width as u64 * e.height as u64 * 4);
    let pool = unsafe { pool_for(d, family)? };
    if d.swaps.get(&sc)?.slots[index].as_ref().is_none_or(|s| s.size < bytes) {
        if let Some(old) = d.swaps.get_mut(&sc)?.slots[index].take() {
            unsafe { free_slot(d, old) };
        }
        let slot = unsafe { new_slot(d, pool, bytes.max(1024 * 1024 * 4))? };
        d.swaps.get_mut(&sc)?.slots[index] = Some(slot);
    }
    // wait for this image's previous draw before touching its buffers
    {
        let slot = d.swaps.get(&sc)?.slots[index].as_ref()?;
        if slot.used && unsafe { (d.f.wait_for_fences)(d.device, 1, &slot.fence, vk::TRUE, 50_000_000) } != vk::Result::SUCCESS {
            return None;
        }
    }
    if d.swaps.get(&sc)?.slots[index].as_ref()?.tex.as_ref().is_none_or(|t| (t.width, t.height) != (frame.width, frame.height)) {
        if let Some(old) = d.swaps.get_mut(&sc)?.slots[index].as_mut()?.tex.take() {
            unsafe { free_tex(d, old) };
        }
        let t = unsafe { new_tex(d, tex_format, frame.width, frame.height)? };
        d.swaps.get_mut(&sc)?.slots[index].as_mut()?.tex = Some(t);
    }
    if let Some((e, _)) = tip {
        if d.swaps.get(&sc)?.slots[index].as_ref()?.tip.as_ref().is_none_or(|t| (t.width, t.height) != (e.width, e.height)) {
            if let Some(old) = d.swaps.get_mut(&sc)?.slots[index].as_mut()?.tip.take() {
                unsafe { free_tex(d, old) };
            }
            let t = unsafe { new_tex(d, tex_format, e.width, e.height)? };
            d.swaps.get_mut(&sc)?.slots[index].as_mut()?.tip = Some(t);
        }
    }
    let layout = d.shared.as_ref()?.layout;
    let f = &d.f;
    let slot = d.swaps.get_mut(&sc)?.slots[index].as_mut()?;
    unsafe {
        (f.reset_fences)(d.device, 1, &slot.fence);
        (f.reset_command_buffer)(slot.cmd, vk::CommandBufferResetFlags::empty());
        (f.begin_command_buffer)(slot.cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT));
        // the images to draw: (texture, pixels, size, buffer offset, where, how big on screen)
        let mut draws: Vec<(&mut Tex, &[u8], (u32, u32), u64, vk::Offset2D, (u32, u32))> =
            vec![(slot.tex.as_mut()?, &frame.pixels[..], (frame.width, frame.height), 0, offset, (dw, dh))];
        if let (Some((e, at)), Some(t)) = (tip, slot.tip.as_mut()) {
            draws.push((t, &e.pixels[..], (e.width, e.height), main_bytes, at, (e.width, e.height)));
        }
        let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
        for (tex, px, (w, h), at, _, _) in draws.iter_mut() {
            if tex.seq == frame.seq && tex.seq != 0 {
                continue;
            }
            std::ptr::copy_nonoverlapping(px.as_ptr(), slot.ptr.add(*at as usize), px.len());
            let old = if tex.seq == 0 { vk::ImageLayout::UNDEFINED } else { vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL };
            let to_dst = vk::ImageMemoryBarrier::default().src_access_mask(vk::AccessFlags::SHADER_READ).dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(old).new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED).dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED).image(tex.image).subresource_range(range);
            (f.cmd_pipeline_barrier)(slot.cmd, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), 0, std::ptr::null(), 0, std::ptr::null(), 1, &to_dst);
            let region = vk::BufferImageCopy::default().buffer_offset(*at)
                .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1))
                .image_extent(vk::Extent3D { width: *w, height: *h, depth: 1 });
            (f.cmd_copy_buffer_to_image)(slot.cmd, slot.buf, tex.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, 1, &region);
            let to_read = vk::ImageMemoryBarrier::default().src_access_mask(vk::AccessFlags::TRANSFER_WRITE).dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL).new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED).dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED).image(tex.image).subresource_range(range);
            (f.cmd_pipeline_barrier)(slot.cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::VERTEX_SHADER | vk::PipelineStageFlags::FRAGMENT_SHADER, vk::DependencyFlags::empty(), 0, std::ptr::null(), 0, std::ptr::null(), 1, &to_read);
            tex.seq = frame.seq.max(1);
        }
        let rp = vk::RenderPassBeginInfo::default().render_pass(render_pass).framebuffer(framebuffer)
            .render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent });
        (f.cmd_begin_render_pass)(slot.cmd, &rp, vk::SubpassContents::INLINE);
        (f.cmd_bind_pipeline)(slot.cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
        for (tex, _, _, _, at, (w, h)) in &draws {
            (f.cmd_bind_descriptor_sets)(slot.cmd, vk::PipelineBindPoint::GRAPHICS, layout, 0, 1, &tex.set, 0, std::ptr::null());
            let vp = vk::Viewport { x: at.x as f32, y: at.y as f32, width: *w as f32, height: *h as f32, min_depth: 0.0, max_depth: 1.0 };
            (f.cmd_set_viewport)(slot.cmd, 0, 1, &vp);
            let sc_rect = vk::Rect2D { offset: *at, extent: vk::Extent2D { width: *w, height: *h } };
            (f.cmd_set_scissor)(slot.cmd, 0, 1, &sc_rect);
            (f.cmd_draw)(slot.cmd, 4, 1, 0, 0);
        }
        drop(draws);
        (f.cmd_end_render_pass)(slot.cmd);
        (f.end_command_buffer)(slot.cmd);
        let stages = vec![vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT; info.wait_semaphore_count as usize];
        let mut submit = vk::SubmitInfo::default();
        submit.wait_semaphore_count = info.wait_semaphore_count;
        submit.p_wait_semaphores = info.p_wait_semaphores;
        submit.p_wait_dst_stage_mask = stages.as_ptr();
        submit.command_buffer_count = 1;
        submit.p_command_buffers = &slot.cmd;
        submit.signal_semaphore_count = 1;
        submit.p_signal_semaphores = &slot.sem;
        if (f.queue_submit)(queue, 1, &submit, slot.fence) != vk::Result::SUCCESS {
            return None;
        }
        slot.used = true;
        input::set_meter(Some(input::Rect { x: offset.x, y: offset.y, w: dw, h: dh, image_w: extent.width, image_h: extent.height }), (frame.width, frame.height), frame.port, frame.scale, frame.place);
        Some(slot.sem)
    }
}

unsafe fn pool_for(d: &mut Dev, family: u32) -> Option<vk::CommandPool> {
    if let Some(p) = d.pools.get(&family) {
        return Some(*p);
    }
    let ci = vk::CommandPoolCreateInfo::default().flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER).queue_family_index(family);
    let mut pool = vk::CommandPool::null();
    if unsafe { (d.f.create_command_pool)(d.device, &ci, std::ptr::null(), &mut pool) } != vk::Result::SUCCESS {
        return None;
    }
    d.pools.insert(family, pool);
    Some(pool)
}

unsafe fn new_slot(d: &Dev, pool: vk::CommandPool, size: u64) -> Option<Slot> {
    let f = &d.f;
    unsafe {
        let ai = vk::CommandBufferAllocateInfo::default().command_pool(pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1);
        let mut cmd = vk::CommandBuffer::null();
        if (f.allocate_command_buffers)(d.device, &ai, &mut cmd) != vk::Result::SUCCESS {
            return None;
        }
        // a command buffer the layer made needs the loader's dispatch pointer like any other
        if let Some(set) = d.set_loader_data {
            set(d.device, cmd.as_raw() as *mut c_void);
        }
        let mut fence = vk::Fence::null();
        (f.create_fence)(d.device, &vk::FenceCreateInfo::default(), std::ptr::null(), &mut fence);
        let mut sem = vk::Semaphore::null();
        (f.create_semaphore)(d.device, &vk::SemaphoreCreateInfo::default(), std::ptr::null(), &mut sem);
        let bi = vk::BufferCreateInfo::default().size(size).usage(vk::BufferUsageFlags::TRANSFER_SRC).sharing_mode(vk::SharingMode::EXCLUSIVE);
        let mut buf = vk::Buffer::null();
        if (f.create_buffer)(d.device, &bi, std::ptr::null(), &mut buf) != vk::Result::SUCCESS {
            return None;
        }
        let mut req = vk::MemoryRequirements::default();
        (f.get_buffer_memory_requirements)(d.device, buf, &mut req);
        let want = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let ty = (0..d.mem.memory_type_count).find(|&i| req.memory_type_bits & (1 << i) != 0 && d.mem.memory_types[i as usize].property_flags.contains(want))?;
        let mi = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty);
        let mut mem = vk::DeviceMemory::null();
        if (f.allocate_memory)(d.device, &mi, std::ptr::null(), &mut mem) != vk::Result::SUCCESS {
            return None;
        }
        (f.bind_buffer_memory)(d.device, buf, mem, 0);
        let mut ptr = std::ptr::null_mut();
        if (f.map_memory)(d.device, mem, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty(), &mut ptr) != vk::Result::SUCCESS {
            return None;
        }
        Some(Slot { cmd, fence, sem, buf, mem, ptr: ptr as *mut u8, size, tex: None, tip: None, used: false })
    }
}
