// SPDX-License-Identifier: GPL-2.0-or-later
//! The few alsa-lib high-level control (hctl) calls motu424-ctl needs, as
//! small owning types over alsa-sys. Every raw pointer is allocated by
//! alsa-lib and freed in Drop.

use alsa_sys as sys;
use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::os::raw::{c_char, c_int, c_long};

const EINVAL: c_int = 22;

/// An alsa-lib error code with its message.
pub struct Error(c_int);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: snd_strerror returns a static string for any code.
        let s = unsafe { CStr::from_ptr(sys::snd_strerror(self.0)) };
        f.write_str(&s.to_string_lossy())
    }
}

fn check(rc: c_int) -> Result<c_int, Error> {
    if rc < 0 {
        Err(Error(rc))
    } else {
        Ok(rc)
    }
}

fn cstr<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        ""
    } else {
        // SAFETY: alsa-lib returns NUL-terminated strings that live as long
        // as the object they came from, which outlives the borrow here.
        unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ElemType {
    Boolean,
    Integer,
    Integer64,
    Enumerated,
    Bytes,
    Other,
}

/// The first card whose driver is `driver` or whose name contains `name`
/// (both case-insensitive), as "hw:N".
pub fn find_motu_card(driver: &str, name: &str) -> Option<String> {
    let mut card: c_int = -1;
    let name = name.to_ascii_lowercase();
    loop {
        // SAFETY: snd_card_next advances an int index.
        if unsafe { sys::snd_card_next(&mut card) } != 0 || card < 0 {
            return None;
        }
        let hw = format!("hw:{card}");
        let c = CString::new(hw.clone()).ok()?;
        let mut ctl: *mut sys::snd_ctl_t = std::ptr::null_mut();
        // SAFETY: opens a control handle, closed below.
        if unsafe { sys::snd_ctl_open(&mut ctl, c.as_ptr(), 0) } < 0 {
            continue;
        }
        let mut info: *mut sys::snd_ctl_card_info_t = std::ptr::null_mut();
        let mut found = false;
        // SAFETY: info is allocated, filled from the open handle, then freed.
        unsafe {
            if sys::snd_ctl_card_info_malloc(&mut info) == 0 {
                if sys::snd_ctl_card_info(ctl, info) == 0 {
                    let drv = cstr(sys::snd_ctl_card_info_get_driver(info));
                    let nm = cstr(sys::snd_ctl_card_info_get_name(info));
                    found = drv.eq_ignore_ascii_case(driver)
                        || nm.to_ascii_lowercase().contains(&name);
                }
                sys::snd_ctl_card_info_free(info);
            }
            sys::snd_ctl_close(ctl);
        }
        if found {
            return Some(hw);
        }
    }
}

/// An open, loaded high-level control handle.
pub struct HCtl(*mut sys::snd_hctl_t);

impl HCtl {
    pub fn open(name: &str) -> Result<Self, Error> {
        let c = CString::new(name).map_err(|_| Error(-EINVAL))?;
        let mut h: *mut sys::snd_hctl_t = std::ptr::null_mut();
        // SAFETY: opens a handle owned by the returned value.
        check(unsafe { sys::snd_hctl_open(&mut h, c.as_ptr(), 0) })?;
        Ok(HCtl(h))
    }

    pub fn load(&self) -> Result<(), Error> {
        // SAFETY: self.0 is a valid open handle.
        check(unsafe { sys::snd_hctl_load(self.0) }).map(|_| ())
    }

    /// Every element, in alsa-lib's order.
    pub fn elems(&self) -> impl Iterator<Item = HElem<'_>> {
        // SAFETY: the element list belongs to the loaded handle.
        let mut cur = unsafe { sys::snd_hctl_first_elem(self.0) };
        std::iter::from_fn(move || {
            if cur.is_null() {
                return None;
            }
            let e = HElem(cur, PhantomData);
            // SAFETY: cur is an element of the loaded handle.
            cur = unsafe { sys::snd_hctl_elem_next(cur) };
            Some(e)
        })
    }

    /// The mixer-interface element called `name`.
    pub fn find(&self, name: &str) -> Option<HElem<'_>> {
        let c = CString::new(name).ok()?;
        let mut id: *mut sys::snd_ctl_elem_id_t = std::ptr::null_mut();
        // SAFETY: id is allocated, filled, used for the lookup and freed.
        unsafe {
            if sys::snd_ctl_elem_id_malloc(&mut id) != 0 {
                return None;
            }
            sys::snd_ctl_elem_id_set_interface(id, sys::SND_CTL_ELEM_IFACE_MIXER);
            sys::snd_ctl_elem_id_set_name(id, c.as_ptr());
            let e = sys::snd_hctl_find_elem(self.0, id);
            sys::snd_ctl_elem_id_free(id);
            (!e.is_null()).then_some(HElem(e, PhantomData))
        }
    }
}

impl Drop for HCtl {
    fn drop(&mut self) {
        // SAFETY: closes the handle opened in `open`.
        unsafe { sys::snd_hctl_close(self.0) };
    }
}

/// One element of an HCtl; borrowed from it.
pub struct HElem<'a>(*mut sys::snd_hctl_elem_t, PhantomData<&'a HCtl>);

impl HElem<'_> {
    pub fn name(&self) -> &str {
        // SAFETY: the element is alive for the borrow of its HCtl.
        cstr(unsafe { sys::snd_hctl_elem_get_name(self.0) })
    }

    pub fn info(&self) -> Result<Info, Error> {
        let mut p: *mut sys::snd_ctl_elem_info_t = std::ptr::null_mut();
        // SAFETY: allocates an info owned by the returned value.
        check(unsafe { sys::snd_ctl_elem_info_malloc(&mut p) })?;
        let info = Info(p);
        // SAFETY: fills the info from a live element.
        check(unsafe { sys::snd_hctl_elem_info(self.0, info.0) })?;
        Ok(info)
    }

    /// The name of enum item `idx`, using `info` as scratch space.
    pub fn item_name(&self, info: &mut Info, idx: u32) -> Option<String> {
        // SAFETY: selects an item in an owned info and refreshes it from the element.
        unsafe {
            sys::snd_ctl_elem_info_set_item(info.0, idx);
            if sys::snd_hctl_elem_info(self.0, info.0) < 0 {
                return None;
            }
            Some(cstr(sys::snd_ctl_elem_info_get_item_name(info.0)).to_string())
        }
    }

    pub fn read(&self) -> Result<Value, Error> {
        let mut p: *mut sys::snd_ctl_elem_value_t = std::ptr::null_mut();
        // SAFETY: allocates a value owned by the returned value.
        check(unsafe { sys::snd_ctl_elem_value_malloc(&mut p) })?;
        let v = Value(p);
        // SAFETY: reads a live element into the owned value.
        check(unsafe { sys::snd_hctl_elem_read(self.0, v.0) })?;
        Ok(v)
    }

    pub fn write(&self, v: &Value) -> Result<(), Error> {
        // SAFETY: writes an owned value to a live element.
        check(unsafe { sys::snd_hctl_elem_write(self.0, v.0) }).map(|_| ())
    }
}

/// An owned snd_ctl_elem_info_t.
pub struct Info(*mut sys::snd_ctl_elem_info_t);

impl Info {
    pub fn elem_type(&self) -> ElemType {
        // SAFETY: reads a field of an owned info.
        match unsafe { sys::snd_ctl_elem_info_get_type(self.0) } {
            sys::SND_CTL_ELEM_TYPE_BOOLEAN => ElemType::Boolean,
            sys::SND_CTL_ELEM_TYPE_INTEGER => ElemType::Integer,
            sys::SND_CTL_ELEM_TYPE_INTEGER64 => ElemType::Integer64,
            sys::SND_CTL_ELEM_TYPE_ENUMERATED => ElemType::Enumerated,
            sys::SND_CTL_ELEM_TYPE_BYTES => ElemType::Bytes,
            _ => ElemType::Other,
        }
    }

    pub fn count(&self) -> u32 {
        // SAFETY: reads a field of an owned info.
        unsafe { sys::snd_ctl_elem_info_get_count(self.0) }
    }

    pub fn min(&self) -> c_long {
        // SAFETY: reads a field of an owned info.
        unsafe { sys::snd_ctl_elem_info_get_min(self.0) }
    }

    pub fn max(&self) -> c_long {
        // SAFETY: reads a field of an owned info.
        unsafe { sys::snd_ctl_elem_info_get_max(self.0) }
    }

    pub fn items(&self) -> u32 {
        // SAFETY: reads a field of an owned info.
        unsafe { sys::snd_ctl_elem_info_get_items(self.0) }
    }
}

impl Drop for Info {
    fn drop(&mut self) {
        // SAFETY: frees the info allocated in HElem::info.
        unsafe { sys::snd_ctl_elem_info_free(self.0) };
    }
}

/// An owned snd_ctl_elem_value_t.
pub struct Value(*mut sys::snd_ctl_elem_value_t);

impl Value {
    pub fn boolean(&self, i: u32) -> bool {
        // SAFETY: reads member i of an owned value.
        unsafe { sys::snd_ctl_elem_value_get_boolean(self.0, i) != 0 }
    }

    pub fn integer(&self, i: u32) -> c_long {
        // SAFETY: reads member i of an owned value.
        unsafe { sys::snd_ctl_elem_value_get_integer(self.0, i) }
    }

    pub fn integer64(&self, i: u32) -> i64 {
        // SAFETY: reads member i of an owned value.
        unsafe { sys::snd_ctl_elem_value_get_integer64(self.0, i) }
    }

    pub fn enumerated(&self, i: u32) -> u32 {
        // SAFETY: reads member i of an owned value.
        unsafe { sys::snd_ctl_elem_value_get_enumerated(self.0, i) }
    }

    pub fn set_boolean(&mut self, i: u32, b: bool) {
        // SAFETY: writes member i of an owned value.
        unsafe { sys::snd_ctl_elem_value_set_boolean(self.0, i, c_long::from(b)) }
    }

    pub fn set_integer(&mut self, i: u32, v: i64) {
        // SAFETY: writes member i of an owned value.
        unsafe { sys::snd_ctl_elem_value_set_integer(self.0, i, v as c_long) }
    }

    pub fn set_enumerated(&mut self, i: u32, v: u32) {
        // SAFETY: writes member i of an owned value.
        unsafe { sys::snd_ctl_elem_value_set_enumerated(self.0, i, v) }
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        // SAFETY: frees the value allocated in HElem::read.
        unsafe { sys::snd_ctl_elem_value_free(self.0) };
    }
}
