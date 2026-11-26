// SPDX-License-Identifier: GPL-2.0-or-later

//! The module parameters.
//!
//! The kernel's Rust `module!` macro only takes integer parameters, and this driver keeps the
//! ALSA ones (`index`, `id` and `enable`, arrays of int, string and bool) and two bools
//! (`configure`, `idle_stream`). So the `struct kernel_param` entries are written here by hand,
//! the way `module_param()` / `module_param_array()` lay them out in C, using the parsers the
//! kernel exports (`param_ops_uint`, `param_ops_bool`, `param_array_ops`, ...). The parameters
//! are spelled, typed and documented exactly as with the C driver, and readable in
//! /sys/module/motu424/parameters/.

use core::{cell::UnsafeCell, ffi::c_void};

use kernel::{bindings, ffi::c_char, module_param::KernelParam};

/// Number of ALSA card slots (`SNDRV_CARDS`, from `CONFIG_SND_MAX_CARDS`).
pub(crate) const SNDRV_CARDS: usize = crate::alsa::sys::SNDRV_CARDS as usize;

/// A parameter's storage. The kernel writes it while loading the module, before `init`, and
/// never again: every parameter is read-only (0444) in sysfs.
#[repr(transparent)]
pub(crate) struct Param<T>(UnsafeCell<T>);

// SAFETY: written only by the kernel's parameter parser before the module's init runs, and
// read-only afterwards.
unsafe impl<T> Sync for Param<T> {}

impl<T: Copy> Param<T> {
    const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }

    /// The parameter's value.
    pub(crate) fn get(&self) -> T {
        // SAFETY: no writer exists once the module runs (see the `Sync` impl).
        unsafe { *self.0.get() }
    }

    const fn arg(&self) -> *mut c_void {
        self.0.get().cast()
    }
}

/// `struct kparam_array`, made `Sync`.
#[repr(transparent)]
struct ParamArray(bindings::kparam_array);

// SAFETY: only the kernel's parameter code reads it.
unsafe impl Sync for ParamArray {}

const S_IRUGO: u16 = 0o444;

const fn param(
    name: &'static [u8],
    ops: *const bindings::kernel_param_ops,
    arg: *mut c_void,
) -> KernelParam {
    KernelParam::new(bindings::kernel_param {
        name: name.as_ptr().cast::<c_char>(),
        // `__this_module` is built by the kernel at load time and outlives the parameters.
        mod_: (&raw const bindings::__this_module).cast_mut(),
        ops,
        perm: S_IRUGO,
        level: -1,
        flags: 0,
        __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 { arg },
    })
}

const fn array_param(name: &'static [u8], arr: &'static ParamArray) -> KernelParam {
    KernelParam::new(bindings::kernel_param {
        name: name.as_ptr().cast::<c_char>(),
        mod_: (&raw const bindings::__this_module).cast_mut(),
        // SAFETY: `param_array_ops` is a static exported by the kernel.
        ops: unsafe { core::ptr::from_ref(&bindings::param_array_ops) },
        perm: S_IRUGO,
        level: -1,
        flags: 0,
        __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 {
            arr: core::ptr::from_ref(&arr.0),
        },
    })
}

const fn array<T>(
    elem: &'static Param<[T; SNDRV_CARDS]>,
    ops: *const bindings::kernel_param_ops,
) -> ParamArray {
    ParamArray(bindings::kparam_array {
        max: SNDRV_CARDS as u32,
        elemsize: core::mem::size_of::<T>() as u32,
        num: core::ptr::null_mut(),
        ops,
        elem: elem.0.get().cast(),
    })
}

/// Copies a string into a fixed array, for a `.modinfo` entry.
const fn bytes<const N: usize>(s: &str) -> [u8; N] {
    let b = s.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i];
        i += 1;
    }
    out
}

/// One `.modinfo` string, as `MODULE_PARM_DESC()` and `__MODULE_PARM_TYPE()` emit them for a
/// loadable module.
macro_rules! modinfo {
    ($id:ident, $s:expr) => {
        #[link_section = ".modinfo"]
        #[used(compiler)]
        static $id: [u8; concat!($s, "\0").len()] = bytes(concat!($s, "\0"));
    };
}

/// Declares one parameter: storage, `kernel_param` entry, type and description.
macro_rules! parameter {
    ($name:ident: $ty:ty = $default:expr, $ops:ident, $ptype:literal, $desc:literal) => {
        #[allow(non_upper_case_globals)]
        pub(crate) static $name: Param<$ty> = Param::new($default);
        const _: () = {
            #[link_section = "__param"]
            #[used(compiler)]
            static P: KernelParam = param(
                concat!(stringify!($name), "\0").as_bytes(),
                // SAFETY: the ops are a static exported by the kernel.
                unsafe { core::ptr::from_ref(&bindings::$ops) },
                $name.arg(),
            );
            modinfo!(T, concat!("parmtype=", stringify!($name), ":", $ptype));
            modinfo!(D, concat!("parm=", stringify!($name), ":", $desc));
        };
    };
}

/// Declares one array parameter of `SNDRV_CARDS` elements.
macro_rules! array_parameter {
    ($name:ident: $ty:ty = $default:expr, $ops:ident, $ptype:literal, $desc:literal) => {
        #[allow(non_upper_case_globals)]
        pub(crate) static $name: Param<[$ty; SNDRV_CARDS]> = Param::new([$default; SNDRV_CARDS]);
        const _: () = {
            static A: ParamArray = array(
                &$name,
                // SAFETY: the ops are a static exported by the kernel.
                unsafe { core::ptr::from_ref(&bindings::$ops) },
            );
            #[link_section = "__param"]
            #[used(compiler)]
            static P: KernelParam = array_param(concat!(stringify!($name), "\0").as_bytes(), &A);
            modinfo!(
                T,
                concat!("parmtype=", stringify!($name), ":array of ", $ptype)
            );
            modinfo!(D, concat!("parm=", stringify!($name), ":", $desc));
        };
    };
}

/// A string parameter's value: a `char *` the kernel allocates and frees.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct CharP(pub(crate) *const c_char);

array_parameter!(index: i32 = -1, param_ops_int, "int",
    "Index value for the MOTU PCI-424 card.");
array_parameter!(id: CharP = CharP(core::ptr::null()), param_ops_charp, "charp",
    "ID string for the MOTU PCI-424 card.");
array_parameter!(enable: bool = true, param_ops_bool, "bool",
    "Enable the MOTU PCI-424 card.");

parameter!(frame_bytes: u32 = 0, param_ops_uint, "uint",
    "Host-ring bytes per frame (96 measured at 48 kHz, see docs/transport.md); 0 = streaming disabled");
parameter!(period_bytes: u32 = 0x1800, param_ops_uint, "uint",
    "Host-ring bytes the DSP fetches per interrupt (0x1800 measured at 48 kHz with irq_frames=0x40)");
parameter!(irq_frames: u32 = 0x40, param_ops_uint, "uint",
    "Frames per interrupt written to +0x60 (0x40 by default; diagnostic, see docs/transport.md)");
parameter!(configure: bool = true, param_ops_bool, "bool",
    "Write the vendor's 48 kHz parameter group at load and the channel map at stream start (default Y)");
parameter!(idle_stream: bool = false, param_ops_bool, "bool",
    "Start the vendor's idle zero-channel stream at load and whenever no PCM stream is open (default N; diagnostic)");
