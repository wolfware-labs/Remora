//! `NtCreateFile` relative to an open directory (Windows). Win32 `CreateFileW` and
//! `CreateDirectoryW` take only paths; this is the parent-relative primitive.

use super::Dir;
use super::security::Descriptor;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

pub(crate) enum Kind {
    Dir,
    File,
}

/// Creates `name` in `parent` with `FILE_CREATE`, so an existing name fails with
/// `AlreadyExists` and a reparse point there is never followed.
pub(crate) fn create(
    parent: &Dir,
    name: &OsStr,
    kind: Kind,
    descriptor: &Descriptor,
) -> io::Result<OwnedHandle> {
    let mut wide: Vec<u16> = name.encode_wide().collect();
    let bytes = u16::try_from(wide.len() * 2)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidFilename, "name too long"))?;
    let object_name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_handle().as_raw_handle(),
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: descriptor.as_ptr(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let (access, options, file_attributes) = match kind {
        Kind::Dir => (FILE_GENERIC_READ | FILE_TRAVERSE, FILE_DIRECTORY_FILE, 0),
        Kind::File => (
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            FILE_NON_DIRECTORY_FILE,
            FILE_ATTRIBUTE_NORMAL,
        ),
    };
    let mut handle = std::ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer refers to a live local (`handle`, `attributes`,
    // `status_block`, and through `attributes` the name buffer `wide` and the
    // descriptor, which outlive the call); the parent handle is open for the duration
    // because `parent` is borrowed. The synchronous option matches the blocking I/O
    // that `std::fs::File` expects.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            file_attributes,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_CREATE,
            options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: a pure conversion of a status code.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    // SAFETY: on success `handle` is a new handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}
