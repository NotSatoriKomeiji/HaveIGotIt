use std::path::Path;

#[cfg(not(windows))]
pub fn read(_: &Path) -> Option<String> {
    None
}

#[cfg(windows)]
pub fn read(path: &Path) -> Option<String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    {
        return None;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(wide.as_ptr(), ptr::null_mut());
        if size == 0 || size > 4 * 1024 * 1024 {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        if GetFileVersionInfoW(wide.as_ptr(), 0, size, buffer.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let mut value: *mut c_void = ptr::null_mut();
        let mut length = 0;
        if VerQueryValueW(
            buffer.as_ptr().cast(),
            [92u16, 0].as_ptr(),
            &mut value,
            &mut length,
        ) == 0
            || value.is_null()
            || length < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
        {
            return None;
        }
        let info = ptr::read_unaligned(value.cast::<VS_FIXEDFILEINFO>());
        if info.dwSignature != 0xfeef04bd {
            return None;
        }
        let (ms, ls) = (info.dwProductVersionMS, info.dwProductVersionLS);
        if ms == 0 && ls == 0 {
            return None;
        }
        Some(format!(
            "{}.{}.{}.{}",
            ms >> 16,
            ms & 65535,
            ls >> 16,
            ls & 65535
        ))
    }
}
