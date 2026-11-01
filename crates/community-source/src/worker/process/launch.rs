//! Explicit stdio-only handle inheritance; std::process::Command does not expose
//! a stable Windows HANDLE_LIST API on this package's supported Rust versions.
use super::{Command, Handle};
use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::io;
use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};

#[repr(C)]
struct SecurityAttributes {
    size: u32,
    descriptor: *mut c_void,
    inherit: i32,
}
#[repr(C)]
struct StartupInfo {
    size: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    position_and_flags: [u32; 8],
    show_window: u16,
    reserved_size: u16,
    reserved_data: *mut u8,
    stdin: Handle,
    stdout: Handle,
    stderr: Handle,
}
#[repr(C)]
struct StartupInfoEx {
    startup: StartupInfo,
    attributes: *mut c_void,
}
#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}
#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn CreatePipe(
        read: *mut Handle,
        write: *mut Handle,
        security: *const SecurityAttributes,
        size: u32,
    ) -> i32;
    fn SetHandleInformation(handle: Handle, mask: u32, flags: u32) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const SecurityAttributes,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn InitializeProcThreadAttributeList(
        list: *mut c_void,
        count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *const c_void,
        size: usize,
        previous: *mut c_void,
        returned: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: *mut c_void);
    fn CreateProcessW(
        application: *const u16,
        command_line: *mut u16,
        process_security: *const c_void,
        thread_security: *const c_void,
        inherit: i32,
        flags: u32,
        environment: *const c_void,
        directory: *const u16,
        startup: *const StartupInfoEx,
        information: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    fn TerminateProcess(process: Handle, code: u32) -> i32;
    fn GetProcessTimes(
        process: Handle,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}

fn security() -> SecurityAttributes {
    SecurityAttributes {
        size: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor: std::ptr::null_mut(),
        inherit: 1,
    }
}
fn pipe(parent_reads: bool) -> io::Result<(File, OwnedHandle)> {
    let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, &security(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let (parent, child) = if parent_reads {
        (read, write)
    } else {
        (write, read)
    };
    let parent = unsafe { File::from_raw_handle(parent) };
    let child = unsafe { OwnedHandle::from_raw_handle(child) };
    if unsafe { SetHandleInformation(parent.as_raw_handle(), 1, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((parent, child))
}

struct Attributes(Vec<usize>);
impl Attributes {
    fn new(handles: &[Handle; 3]) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0; bytes.div_ceil(std::mem::size_of::<usize>())];
        if unsafe { InitializeProcThreadAttributeList(storage.as_mut_ptr() as _, 1, 0, &mut bytes) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = Self(storage);
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.0.as_mut_ptr() as _,
                0,
                0x0002_0002,
                handles.as_ptr() as _,
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(attributes)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.0.as_mut_ptr() as _);
        }
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) || value.len() >= 32767 {
        return Err(super::protocol_error());
    }
    value.push(0);
    Ok(value)
}
fn quoted_word(value: &OsStr, line: &mut Vec<u16>) -> io::Result<()> {
    line.push(b'"' as u16);
    let mut slashes = 0;
    for unit in value.encode_wide() {
        if unit == 0 {
            return Err(super::protocol_error());
        }
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        line.extend(std::iter::repeat_n(
            b'\\' as u16,
            if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        line.push(unit);
    }
    line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    line.push(b'"' as u16);
    Ok(())
}

pub(super) struct ChildProcess {
    process: OwnedHandle,
    process_id: u32,
    pub stdin: Option<File>,
    pub stdout: Option<File>,
    pub started_at_unix_ms: Option<i64>,
}
pub(super) struct ProcessExit(u32);
impl ProcessExit {
    pub fn code(&self) -> Option<i32> {
        Some(self.0 as i32)
    }
}
impl AsRawHandle for ChildProcess {
    fn as_raw_handle(&self) -> Handle {
        self.process.as_raw_handle()
    }
}
impl ChildProcess {
    pub fn spawn(command: &Command) -> io::Result<Self> {
        let application = wide(command.get_program())?;
        let mut line = Vec::new();
        quoted_word(command.get_program(), &mut line)?;
        for argument in command.get_args() {
            line.push(b' ' as u16);
            quoted_word(argument, &mut line)?;
        }
        if line.len() >= 32767 {
            return Err(super::protocol_error());
        }
        line.push(0);
        let mut environment_entries: Vec<_> = command
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| {
                    let mut entry = key.to_os_string();
                    entry.push("=");
                    entry.push(value);
                    entry
                })
            })
            .collect();
        environment_entries.sort_by_key(|entry| entry.to_string_lossy().to_uppercase());
        let mut environment = Vec::new();
        for entry in environment_entries {
            environment.extend(wide(&entry)?);
        }
        environment.push(0);
        if environment.len() == 1 {
            environment.push(0);
        }
        if environment.len() > 65536 {
            return Err(super::protocol_error());
        }
        let (stdin, child_stdin) = pipe(false)?;
        let (stdout, child_stdout) = pipe(true)?;
        let null = wide(OsStr::new("NUL"))?;
        let stderr = unsafe {
            CreateFileW(
                null.as_ptr(),
                0x4000_0000,
                3,
                &security(),
                3,
                0x80,
                std::ptr::null_mut(),
            )
        };
        if stderr as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        let stderr = unsafe { OwnedHandle::from_raw_handle(stderr) };
        let handles = [
            child_stdin.as_raw_handle(),
            child_stdout.as_raw_handle(),
            stderr.as_raw_handle(),
        ];
        let mut attributes = Attributes::new(&handles)?;
        let mut startup: StartupInfoEx = unsafe { std::mem::zeroed() };
        startup.startup.size = std::mem::size_of::<StartupInfoEx>() as u32;
        startup.startup.position_and_flags[7] = 0x100;
        startup.startup.stdin = handles[0];
        startup.startup.stdout = handles[1];
        startup.startup.stderr = handles[2];
        startup.attributes = attributes.0.as_mut_ptr() as _;
        let mut information: ProcessInformation = unsafe { std::mem::zeroed() };
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                super::CREATE_NO_WINDOW | 0x0008_0000 | 0x0000_0400,
                environment.as_ptr() as _,
                std::ptr::null(),
                &startup,
                &mut information,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        let process = unsafe { OwnedHandle::from_raw_handle(information.process) };
        let _thread = unsafe { OwnedHandle::from_raw_handle(information.thread) };
        let (mut creation, mut exit, mut kernel, mut user) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        let started_at_unix_ms = if unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } != 0
        {
            let ticks = ((creation.high as u64) << 32) | creation.low as u64;
            Some(
                (ticks / 10_000)
                    .saturating_sub(11_644_473_600_000)
                    .min(i64::MAX as u64) as i64,
            )
        } else {
            None
        };
        Ok(Self {
            process,
            process_id: information.process_id,
            stdin: Some(stdin),
            stdout: Some(stdout),
            started_at_unix_ms,
        })
    }
    pub fn id(&self) -> u32 {
        self.process_id
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
        self.wait_bounded(0)
    }
    pub fn wait_bounded(&mut self, milliseconds: u32) -> io::Result<Option<ProcessExit>> {
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), milliseconds) } {
            0 => {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ProcessExit(code)))
            }
            0x102 => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
    pub fn kill(&self) -> io::Result<()> {
        if unsafe { TerminateProcess(self.process.as_raw_handle(), 0xE001_0002) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
