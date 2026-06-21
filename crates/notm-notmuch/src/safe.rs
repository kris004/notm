use std::{ffi::CStr, os::raw::c_char, path::Path};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// Convert a NUL-terminated C string pointer to an owned Rust string.
///
/// # Safety
///
/// `ptr` must be either null or point to a valid NUL-terminated string for the
/// duration of this call.
pub unsafe fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Take ownership of a malloc-allocated C string and free it with `libc::free`.
///
/// # Safety
///
/// `ptr` must be null or a pointer returned by a C allocator compatible with
/// `libc::free`, and must not be used again after this function returns.
pub unsafe fn take_malloc_string(ptr: *mut c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        let value = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { libc::free(ptr.cast()) };
        value
    }
}

#[cfg(unix)]
pub fn path_to_cstring(path: &Path) -> Result<std::ffi::CString, std::ffi::NulError> {
    std::ffi::CString::new(path.as_os_str().as_bytes())
}

#[cfg(not(unix))]
pub fn path_to_cstring(path: &Path) -> Result<std::ffi::CString, std::ffi::NulError> {
    std::ffi::CString::new(path.to_string_lossy().as_bytes())
}
