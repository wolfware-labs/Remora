//! `NtCreateFile` relative to an open directory (Windows). Win32 `CreateFileW` and
//! `CreateDirectoryW` take only paths; this is the parent-relative primitive for
//! both creating (`remora_fs::private`) and opening (`sys::at`).

use super::Dir;
use super::security::Descriptor;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    NTSTATUS, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, STATUS_FILE_IS_A_DIRECTORY,
    STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_INVALID, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, SYNCHRONIZE,
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
    let (access, options, file_attributes) = match kind {
        Kind::Dir => (FILE_GENERIC_READ | FILE_TRAVERSE, FILE_DIRECTORY_FILE, 0),
        Kind::File => (
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            FILE_NON_DIRECTORY_FILE,
            FILE_ATTRIBUTE_NORMAL,
        ),
    };
    let request = Request {
        access,
        options,
        disposition: FILE_CREATE,
        file_attributes,
        descriptor: Some(descriptor),
    };
    match call(parent, name, &request) {
        // A file over an existing directory is refused as a directory, not as a
        // name collision; with FILE_CREATE either way the name exists.
        Err(STATUS_FILE_IS_A_DIRECTORY) => Err(io::ErrorKind::AlreadyExists.into()),
        result => result.map_err(status_error),
    }
}

/// Opens the existing `name` in `parent` itself, a reparse point included, with
/// `access` and the extra create `options` (`FILE_DIRECTORY_FILE` and so on). Every
/// share mode is granted (spec 5.9 rule 1). A file where `FILE_DIRECTORY_FILE`
/// asked for a directory is `NotADirectory`, the reverse `IsADirectory`.
pub(crate) fn open(
    parent: &Dir,
    name: &OsStr,
    access: u32,
    options: u32,
) -> io::Result<OwnedHandle> {
    let request = Request {
        access,
        options,
        disposition: FILE_OPEN,
        file_attributes: 0,
        descriptor: None,
    };
    match call(parent, name, &request) {
        Err(STATUS_NOT_A_DIRECTORY) => Err(io::ErrorKind::NotADirectory.into()),
        Err(STATUS_FILE_IS_A_DIRECTORY) => Err(io::ErrorKind::IsADirectory.into()),
        result => result.map_err(status_error),
    }
}

struct Request<'a> {
    access: u32,
    options: u32,
    disposition: u32,
    file_attributes: u32,
    descriptor: Option<&'a Descriptor>,
}

fn call(parent: &Dir, name: &OsStr, request: &Request<'_>) -> Result<OwnedHandle, NTSTATUS> {
    let mut wide: Vec<u16> = name.encode_wide().collect();
    // A name longer than a UNICODE_STRING can hold: STATUS_OBJECT_NAME_INVALID.
    let bytes = u16::try_from(wide.len() * 2).map_err(|_| STATUS_OBJECT_NAME_INVALID)?;
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
        SecurityDescriptor: request
            .descriptor
            .map_or(std::ptr::null(), Descriptor::as_ptr),
        SecurityQualityOfService: std::ptr::null(),
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
            request.access | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            request.file_attributes,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            request.disposition,
            request.options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(status);
    }
    // SAFETY: on success `handle` is a new handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// The Win32 error for an NT status.
pub(crate) fn status_error(status: NTSTATUS) -> io::Error {
    // SAFETY: a pure conversion of a status code.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}
