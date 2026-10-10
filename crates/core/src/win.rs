// Copyright (c) 2026 Dr.-Ing. Axel Schild
// SPDX-License-Identifier: MIT

//! Thin Win32 layer: bulk directory enumeration, volume info and the NTFS USN journal.
//!
//! Bindings are declared by hand and buffers are parsed by offset, so the layouts
//! we depend on are visible right here instead of hidden behind a bindings crate.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

// A Win32 HANDLE is an opaque pointer-sized value. Functions that return one
// signal failure with INVALID_HANDLE_VALUE, which is -1.
type Handle = *mut c_void;
const INVALID: Handle = -1isize as Handle;

// Arguments for CreateFileW.
// - FILE_LIST_DIRECTORY / FILE_READ_ATTRIBUTES: the only rights we ask for. Asking
//   for less means more folders can be opened.
// - SHARE_ALL (read | write | delete): we never lock anyone else out of a folder.
// - FILE_FLAG_BACKUP_SEMANTICS: required to open a *directory* rather than a file.
// - FILE_ID_BOTH_DIRECTORY_INFO: the "information class" that makes
//   GetFileInformationByHandleEx return whole directory listings.
const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SHARE_ALL: u32 = 0x7;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_ID_BOTH_DIRECTORY_INFO: u32 = 10;

// File attribute bits as they appear in listings.
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_REPARSE: u32 = 0x400;

// Win32 error codes this program reacts to. Everything else is passed through.
pub const ERROR_FILE_NOT_FOUND: u32 = 2;
pub const ERROR_PATH_NOT_FOUND: u32 = 3;
pub const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_NO_MORE_FILES: u32 = 18;
const ERROR_HANDLE_EOF: u32 = 38;

// Control codes for DeviceIoControl on a volume handle.
// - QUERY returns the journal's id and the range of records it currently holds.
// - READ returns records, but needs a volume handle only administrators can open.
// - READ_UNPRIVILEGED returns the same records to ordinary users (Windows 10+).
const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;
const FSCTL_READ_UNPRIVILEGED_USN_JOURNAL: u32 = 0x0009_03AB;

// The few Win32 functions used, declared by hand. They all return 0 (FALSE) on
// failure; the reason is then available from GetLastError().
#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        sa: *mut c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CloseHandle(h: Handle) -> i32;
    fn GetLastError() -> u32;
    fn GetFileInformationByHandleEx(h: Handle, class: u32, buf: *mut c_void, size: u32) -> i32;
    fn GetFileInformationByHandle(h: Handle, info: *mut [u32; 13]) -> i32;
    fn DeviceIoControl(
        h: Handle,
        code: u32,
        inbuf: *const c_void,
        insize: u32,
        outbuf: *mut c_void,
        outsize: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn GetLogicalDrives() -> u32;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetVolumeInformationW(
        root: *const u16,
        name: *mut u16,
        name_len: u32,
        serial: *mut u32,
        max_component: *mut u32,
        flags: *mut u32,
        fs: *mut u16,
        fs_len: u32,
    ) -> i32;
    fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    fn SetErrorMode(mode: u32) -> u32;
    fn GetCurrentProcess() -> Handle;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(token: Handle, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(
        hwnd: Handle,
        verb: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> isize;
}

/// Converts to the NUL-terminated UTF-16 that the "W" family of Win32 functions expects.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `\\?\`-prefixed wide path so we are not limited to MAX_PATH.
fn long_path(p: &str) -> Vec<u16> {
    if p.starts_with(r"\\?\") {
        wide(p)
    } else if let Some(unc) = p.strip_prefix(r"\\") {
        wide(&format!(r"\\?\UNC\{unc}"))
    } else {
        wide(&format!(r"\\?\{p}"))
    }
}

/// Joins a folder path and a name with exactly one backslash. A drive root such
/// as `C:\` already ends in one, every other folder path does not.
pub fn join(dir: &str, name: &str) -> String {
    let mut s = String::with_capacity(dir.len() + name.len() + 1);
    s.push_str(dir);
    if !dir.ends_with('\\') {
        s.push('\\');
    }
    s.push_str(name);
    s
}

// Little-endian readers. The kernel hands back packed structures in a byte
// buffer; these pick single fields out of it by offset.
#[inline]
fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
#[inline]
fn rd_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// FILETIME (100 ns ticks since 1601) to unix seconds.
#[inline]
fn filetime_to_unix(ft: u64) -> i64 {
    (ft as i64 - 116_444_736_000_000_000) / 10_000_000
}

/// One directory entry exactly as the filesystem reported it.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub name: String,
    pub is_dir: bool,
    /// Logical size in bytes: what Explorer calls "Size". Always 0 for folders.
    pub size: u64,
    /// Bytes actually reserved on disk ("Size on disk"). Smaller than `size` for
    /// compressed, sparse or cloud-only files, slightly larger for ordinary ones.
    pub alloc: u64,
    /// Last write time in unix seconds.
    pub mtime: i64,
    /// NTFS file reference number: an id that is unique on the volume and stays the
    /// same when the item is renamed. The change journal refers to items by it.
    pub frn: u64,
    /// Raw FILE_ATTRIBUTE_* bits.
    pub attrs: u32,
}

impl RawEntry {
    /// Junctions / symlinks: recorded, never followed.
    pub fn is_reparse(&self) -> bool {
        self.attrs & ATTR_REPARSE != 0
    }
}

/// Reusable bulk directory reader. One per worker thread.
pub struct DirLister {
    buf: Vec<u64>, // u64 keeps the buffer 8-byte aligned for the kernel structs
}

impl Default for DirLister {
    fn default() -> Self {
        Self::new()
    }
}

impl DirLister {
    pub fn new() -> Self {
        Self {
            buf: vec![0u64; 32 * 1024],
        } // 256 KiB
    }

    /// Lists one directory. Each kernel round trip fills the whole buffer with
    /// FILE_ID_BOTH_DIR_INFO records (name, sizes, times, attributes, file id).
    pub fn list(&mut self, path: &str) -> Result<Vec<RawEntry>, u32> {
        let w = long_path(path);
        let byte_len = self.buf.len() * 8;
        unsafe {
            // Open the folder itself (not a search handle, as FindFirstFile would).
            let h = CreateFileW(
                w.as_ptr(),
                FILE_LIST_DIRECTORY,
                SHARE_ALL,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            );
            if h == INVALID {
                return Err(GetLastError());
            }
            // Every call fills the buffer with as many entries as fit. The listing is
            // complete when the call fails with ERROR_NO_MORE_FILES.
            let mut out = Vec::new();
            loop {
                let ok = GetFileInformationByHandleEx(
                    h,
                    FILE_ID_BOTH_DIRECTORY_INFO,
                    self.buf.as_mut_ptr() as *mut c_void,
                    byte_len as u32,
                );
                if ok == 0 {
                    let err = GetLastError();
                    CloseHandle(h);
                    return if err == ERROR_NO_MORE_FILES { Ok(out) } else { Err(err) };
                }
                let base = self.buf.as_ptr() as *const u8;
                let bytes = std::slice::from_raw_parts(base, byte_len);
                // Walk the FILE_ID_BOTH_DIR_INFO records packed into the buffer.
                // Byte offsets inside one record:
                //     0  NextEntryOffset   u32   distance to the next record, 0 on the last one
                //    24  LastWriteTime     u64   FILETIME
                //    40  EndOfFile         u64   logical size
                //    48  AllocationSize    u64   size on disk
                //    56  FileAttributes    u32
                //    60  FileNameLength    u32   in bytes, not characters
                //    96  FileId            u64   file reference number
                //   104  FileName          UTF-16, not NUL-terminated
                let mut off = 0usize;
                loop {
                    let next = rd_u32(bytes, off) as usize;
                    let name_len = rd_u32(bytes, off + 60) as usize;
                    let name16 = std::slice::from_raw_parts(base.add(off + 104) as *const u16, name_len / 2);
                    // 46 is '.', so this recognises the `.` and `..` pseudo-entries.
                    let dot = name16 == [46] || name16 == [46, 46];
                    if !dot {
                        let attrs = rd_u32(bytes, off + 56);
                        out.push(RawEntry {
                            name: String::from_utf16_lossy(name16),
                            is_dir: attrs & ATTR_DIRECTORY != 0,
                            size: rd_u64(bytes, off + 40),
                            alloc: rd_u64(bytes, off + 48),
                            mtime: filetime_to_unix(rd_u64(bytes, off + 24)),
                            frn: rd_u64(bytes, off + 96),
                            attrs,
                        });
                    }
                    if next == 0 {
                        break;
                    }
                    off += next;
                }
            }
        }
    }
}

/// (file id, attributes, mtime) of a single path; used for scan roots.
pub fn stat_dir(path: &str) -> Result<(u64, u32, i64), u32> {
    let w = long_path(path);
    unsafe {
        let h = CreateFileW(
            w.as_ptr(),
            FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        );
        if h == INVALID {
            return Err(GetLastError());
        }
        // BY_HANDLE_FILE_INFORMATION is thirteen consecutive u32 fields. We use
        // [0] attributes, [5] and [6] last write time (low, high half) and
        // [11] and [12] the file index (high, low half), which is the reference number.
        let mut info = [0u32; 13];
        let ok = GetFileInformationByHandle(h, &mut info);
        let err = GetLastError();
        CloseHandle(h);
        if ok == 0 {
            return Err(err);
        }
        let frn = ((info[11] as u64) << 32) | info[12] as u64;
        let mtime = filetime_to_unix(((info[6] as u64) << 32) | info[5] as u64);
        Ok((frn, info[0], mtime))
    }
}

/// What we need to know about a volume: for the drive list in the UI, and to
/// decide whether a change journal can exist (NTFS only).
#[derive(Debug, Clone, Default)]
pub struct VolumeInfo {
    pub label: String,
    pub fs: String,
    pub serial: u32,
    /// GetDriveType: 2 removable, 3 fixed, 4 network, 5 optical, 6 ramdisk.
    pub kind: u32,
    pub total: u64,
    pub free: u64,
}

/// NUL-terminated UTF-16 buffer to `String`.
fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Stops Windows from popping "no disk in drive" dialogs for empty card readers.
pub fn quiet_errors() {
    unsafe {
        // SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX
        SetErrorMode(0x0001 | 0x8000);
    }
}

/// `root` must be a volume root such as `C:\`.
pub fn volume_info(root: &str) -> Option<VolumeInfo> {
    let w = wide(root);
    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    let mut v = VolumeInfo::default();
    unsafe {
        v.kind = GetDriveTypeW(w.as_ptr());
        let ok = GetVolumeInformationW(
            w.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut v.serial,
            null_mut(),
            null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        );
        if ok == 0 {
            return None;
        }
        let mut avail = 0u64;
        GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut v.total, &mut v.free);
    }
    v.label = from_wide(&label);
    v.fs = from_wide(&fs);
    Some(v)
}

/// All drive roots currently mounted (`C:\`, `D:\`, ...).
pub fn drives() -> Vec<String> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| format!("{}:\\", (b'A' + i) as char))
        .collect()
}

/// True when this process runs with administrator rights.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: Handle = null_mut();
        // 0x0008 is TOKEN_QUERY. Information class 20 is TokenElevation, which is a
        // single u32: non-zero when elevated.
        if OpenProcessToken(GetCurrentProcess(), 0x0008, &mut token) == 0 {
            return false;
        }
        let mut elevated = 0u32;
        let mut ret = 0u32;
        let ok = GetTokenInformation(token, 20, &mut elevated as *mut u32 as *mut c_void, 4, &mut ret);
        CloseHandle(token);
        ok != 0 && elevated != 0
    }
}

/// ShellExecute with an optional verb ("runas" triggers the UAC prompt).
pub fn shell_execute(verb: Option<&str>, file: &str, params: Option<&str>) -> bool {
    let verb_w = verb.map(wide);
    let file_w = wide(file);
    let params_w = params.map(wide);
    let r = unsafe {
        ShellExecuteW(
            null_mut(),
            verb_w.as_ref().map_or(null(), |v| v.as_ptr()),
            file_w.as_ptr(),
            params_w.as_ref().map_or(null(), |v| v.as_ptr()),
            null(),
            1,
        )
    };
    // ShellExecute reports success as any value above 32.
    r > 32
}

/// One change-journal record, reduced to what the index needs.
#[derive(Debug, Clone, Copy)]
pub struct UsnRecord {
    pub frn: u64,
    /// Reference number of the folder that contains the changed item. This is the
    /// one field the scanner acts on: it says which folder to list again.
    pub parent_frn: u64,
    /// USN_REASON_* bits: created, deleted, data extended, renamed and so on.
    pub reason: u32,
    pub attrs: u32,
}

#[derive(Debug)]
pub enum JournalError {
    /// The volume has no journal we can open (not NTFS, network drive, no rights).
    Unavailable(u32),
    /// Our checkpoint is no longer covered by the journal; a full rescan is needed.
    Expired,
    /// Any other Win32 error while reading.
    Io(u32),
}

/// An open handle on a volume's USN change journal.
pub struct Journal {
    h: Handle,
    /// Identifies this journal instance. Windows assigns a new id when a journal is
    /// deleted and recreated, which makes every stored checkpoint meaningless.
    pub id: u64,
    /// Oldest record still available. The journal has a fixed size and overwrites
    /// its oldest records, so a checkpoint older than this cannot be replayed.
    pub first_usn: i64,
    /// Position the next record will get: the journal's notion of "now".
    pub next_usn: i64,
    /// True when opened with full volume read access (elevated).
    pub privileged: bool,
}

// The handle is a reference to a kernel object and can be used from any thread.
unsafe impl Send for Journal {}

impl Drop for Journal {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.h);
        }
    }
}

impl Journal {
    /// Opens the journal of drive `letter`. Tries a privileged volume handle first,
    /// then falls back to the unprivileged access Windows 10+ offers.
    pub fn open(letter: char) -> Result<Journal, JournalError> {
        let path = wide(&format!(r"\\.\{letter}:"));
        let mut last = 0u32;
        // GENERIC_READ, then SYNCHRONIZE|FILE_TRAVERSE, then SYNCHRONIZE|FILE_READ_ATTRIBUTES.
        for (access, privileged) in [(0x8000_0000u32, true), (0x0010_0020, false), (0x0010_0080, false)] {
            unsafe {
                let h = CreateFileW(
                    path.as_ptr(),
                    access,
                    SHARE_ALL,
                    null_mut(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                );
                if h == INVALID {
                    last = GetLastError();
                    continue;
                }
                // USN_JOURNAL_DATA as u64 fields: [0] UsnJournalID, [1] FirstUsn,
                // [2] NextUsn, followed by fields we do not need.
                let mut data = [0u64; 10];
                let mut ret = 0u32;
                let ok = DeviceIoControl(
                    h,
                    FSCTL_QUERY_USN_JOURNAL,
                    null(),
                    0,
                    data.as_mut_ptr() as *mut c_void,
                    80,
                    &mut ret,
                    null_mut(),
                );
                if ok == 0 {
                    last = GetLastError();
                    CloseHandle(h);
                    continue;
                }
                return Ok(Journal {
                    h,
                    id: data[0],
                    first_usn: data[1] as i64,
                    next_usn: data[2] as i64,
                    privileged,
                });
            }
        }
        Err(JournalError::Unavailable(last))
    }

    /// Streams every record with `usn >= start` to `f` and returns the USN to resume from.
    pub fn read(&self, start: i64, mut f: impl FnMut(UsnRecord)) -> Result<i64, JournalError> {
        if start < self.first_usn {
            return Err(JournalError::Expired);
        }
        let mut out = vec![0u64; 128 * 1024]; // 1 MiB
        let out_bytes = (out.len() * 8) as u32;
        let code = if self.privileged {
            FSCTL_READ_USN_JOURNAL
        } else {
            FSCTL_READ_UNPRIVILEGED_USN_JOURNAL
        };
        let mut usn = start;
        loop {
            // READ_USN_JOURNAL_DATA_V1: StartUsn, ReasonMask|ReturnOnlyOnClose, Timeout,
            // BytesToWaitFor, UsnJournalID, MinMajorVersion|MaxMajorVersion (both 2).
            let input: [u64; 6] = [usn as u64, 0xFFFF_FFFF, 0, 0, self.id, 2 | (2 << 16)];
            let mut ret = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    self.h,
                    code,
                    input.as_ptr() as *const c_void,
                    48,
                    out.as_mut_ptr() as *mut c_void,
                    out_bytes,
                    &mut ret,
                    null_mut(),
                )
            };
            if ok == 0 {
                let err = unsafe { GetLastError() };
                return match err {
                    ERROR_HANDLE_EOF => Ok(usn),
                    1181 | 1178 | 1179 => Err(JournalError::Expired), // entry deleted / delete in progress / not active
                    e => Err(JournalError::Io(e)),
                };
            }
            let ret = ret as usize;
            if ret < 8 {
                return Ok(usn);
            }
            let bytes = unsafe { std::slice::from_raw_parts(out.as_ptr() as *const u8, ret) };
            // The output begins with the USN to continue from. After that come packed
            // USN_RECORD_V2 structures; byte offsets inside one record:
            //    0  RecordLength               u32
            //    4  MajorVersion               u16   we only understand version 2
            //    8  FileReferenceNumber        u64
            //   16  ParentFileReferenceNumber  u64
            //   40  Reason                     u32
            //   52  FileAttributes             u32
            let next = rd_u64(bytes, 0) as i64;
            let mut off = 8usize;
            while off + 60 <= ret {
                let len = rd_u32(bytes, off) as usize;
                if len == 0 {
                    break;
                }
                if rd_u16(bytes, off + 4) == 2 {
                    f(UsnRecord {
                        frn: rd_u64(bytes, off + 8),
                        parent_frn: rd_u64(bytes, off + 16),
                        reason: rd_u32(bytes, off + 40),
                        attrs: rd_u32(bytes, off + 52),
                    });
                }
                off += len;
            }
            // Only the 8-byte header came back, or the position did not move: caught up.
            if ret == 8 || next <= usn {
                return Ok(next.max(usn));
            }
            usn = next;
            // Stop at what was "now" when the journal was opened. On a busy disk new
            // records arrive all the time and we would otherwise never finish.
            if usn >= self.next_usn {
                return Ok(usn);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(w: &[u16]) -> String {
        String::from_utf16_lossy(&w[..w.len() - 1])
    }

    #[test]
    fn join_adds_exactly_one_separator() {
        assert_eq!(join(r"C:\", "Users"), r"C:\Users");
        assert_eq!(join(r"C:\Users", "Me"), r"C:\Users\Me");
    }

    #[test]
    fn long_paths_get_the_extended_prefix() {
        assert_eq!(text(&long_path(r"C:\Temp")), r"\\?\C:\Temp");
        assert_eq!(text(&long_path(r"\\server\share\dir")), r"\\?\UNC\server\share\dir");
        assert_eq!(
            text(&long_path(r"\\?\C:\Temp")),
            r"\\?\C:\Temp",
            "an existing prefix is left alone"
        );
        assert_eq!(long_path("x").last(), Some(&0), "wide strings are NUL-terminated");
    }

    #[test]
    fn filetime_converts_to_unix_seconds() {
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), 0);
        assert_eq!(filetime_to_unix(116_444_736_000_000_000 + 10_000_000 * 86_400), 86_400);
    }

    #[test]
    fn listing_reports_sizes_kinds_and_skips_dot_entries() {
        let dir = std::env::temp_dir().join(format!("canopy-win-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("file.bin"), vec![0u8; 1234]).unwrap();

        let mut entries = DirLister::new().list(&dir.to_string_lossy()).expect("list directory");
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["file.bin", "sub"], "`.` and `..` are not reported");
        assert_eq!((entries[0].is_dir, entries[0].size), (false, 1234));
        assert!(
            entries[0].alloc >= entries[0].size || entries[0].alloc == 0,
            "allocation covers the data unless it is stored inline"
        );
        assert!(entries[0].mtime > 1_600_000_000, "timestamps are unix seconds");
        assert!(entries[1].is_dir && !entries[1].is_reparse());

        let (_, attrs, _) = stat_dir(&dir.to_string_lossy()).expect("stat directory");
        assert_ne!(attrs & ATTR_DIRECTORY, 0);
        std::fs::remove_dir_all(&dir).unwrap();

        let missing = DirLister::new().list(&dir.to_string_lossy());
        assert!(matches!(missing, Err(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)));
    }

    #[test]
    fn drives_are_listed_as_roots() {
        let all = drives();
        assert!(!all.is_empty());
        assert!(all.iter().all(|d| d.len() == 3 && d.ends_with(":\\")));
    }
}
