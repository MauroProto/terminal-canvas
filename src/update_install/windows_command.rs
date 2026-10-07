//! Small native launcher for update verification, not a replacement for
//! `std::process::Command`. It accepts absolute `.exe` paths, ordinary CRT
//! arguments, an optional absolute working directory, and inherited environment
//! variables with explicit overrides/removals. There are no raw shell arguments.
//!
//! Windows 10's JOB_LIST process attribute assigns the private kill-on-close job
//! before the initial thread can run. The additional suspended start lets us
//! verify that exact association while a guard already owns every process handle.

use std::cmp::Ordering;
use std::ffi::{c_void, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

type Handle = *mut c_void;
const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
const PROC_THREAD_ATTRIBUTE_JOB_LIST: usize = 0x0002_000d;
const DUPLICATE_SAME_ACCESS: u32 = 2;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const WAIT_FAILED: u32 = u32::MAX;
const REAP_TIMEOUT_MS: u32 = 5_000;
const MAX_COMMAND_LINE_UNITS: usize = 32_767;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(
        job: Handle,
        information_class: i32,
        information: *const c_void,
        information_length: u32,
    ) -> i32;
    fn TerminateJobObject(job: Handle, exit_code: u32) -> i32;
    fn IsProcessInJob(process: Handle, job: Handle, result: *mut i32) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn DuplicateHandle(
        source_process: Handle,
        source: Handle,
        target_process: Handle,
        target: *mut Handle,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    fn InitializeProcThreadAttributeList(
        list: *mut c_void,
        count: u32,
        flags: u32,
        bytes: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *mut c_void,
        bytes: usize,
        previous: *mut c_void,
        returned_bytes: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: *mut c_void);
    fn CreateProcessW(
        application: *const u16,
        command_line: *mut u16,
        process_attributes: *const c_void,
        thread_attributes: *const c_void,
        inherit_handles: i32,
        flags: u32,
        environment: *const c_void,
        directory: *const u16,
        startup: *const StartupInfo,
        information: *mut ProcessInformation,
    ) -> i32;
    fn ResumeThread(thread: Handle) -> u32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, exit_code: *mut u32) -> i32;
    fn TerminateProcess(process: Handle, exit_code: u32) -> i32;
    fn CompareStringOrdinal(
        left: *const u16,
        left_length: i32,
        right: *const u16,
        right_length: i32,
        ignore_case: i32,
    ) -> i32;
    #[cfg(test)]
    fn CloseHandle(handle: Handle) -> i32;
}

#[repr(C)]
struct StartupInfo {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved_bytes: u16,
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
struct BasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Default)]
struct ExtendedLimitInformation {
    basic: BasicLimitInformation,
    io_counters: [u64; 6],
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

struct Job(OwnedHandle);

impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: null security attributes/name request a private, unnamed,
        // noninheritable job. A successful result transfers one owned handle.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a valid handle owned by this caller.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let limits = ExtendedLimitInformation {
            basic: BasicLimitInformation {
                limit_flags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: the repr(C) buffer has the documented extended-limit layout,
        // remains alive during the call, and belongs to our private live job.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&limits as *const ExtendedLimitInformation).cast(),
                std::mem::size_of::<ExtendedLimitInformation>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn terminate(&self) {
        // SAFETY: this handle owns only the verification process's private job.
        unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.terminate();
        // OwnedHandle closes the sole job handle after this method returns.
    }
}

struct Attributes {
    // usize alignment is sufficient for the Win32 opaque attribute-list buffer.
    // Vec<u8> would not provide that alignment guarantee.
    storage: Vec<usize>,
}

impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: the documented sizing call uses a null buffer and writes only
        // to this live SIZE_T. Failure is expected for this first call.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let units = bytes
            .checked_add(std::mem::size_of::<usize>() - 1)
            .ok_or_else(|| invalid("Process attribute allocation is too large"))?
            / std::mem::size_of::<usize>();
        let mut storage = vec![0usize; units];
        // SAFETY: storage is aligned and at least the requested byte size; it
        // does not move or resize after initialization until deletion in Drop.
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut bytes)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage })
    }

    fn pointer(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr().cast()
    }

    fn set<const N: usize>(
        &mut self,
        attribute: usize,
        handles: &mut [Handle; N],
    ) -> io::Result<()> {
        // SAFETY: the initialized list receives a correctly sized HANDLE array.
        // The caller keeps that array and each handle alive until the list is
        // deleted. Neither reserved output pointer is needed.
        if unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute,
                handles.as_mut_ptr().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: this buffer was successfully initialized and has not been
        // resized or deleted; deletion happens before storage is deallocated.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

fn inheritable_copy(file: &File) -> io::Result<OwnedHandle> {
    let mut handle = std::ptr::null_mut();
    // SAFETY: the source File is live; both process pseudo-handles name this
    // process. The output receives a new real handle with identical access.
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle(),
            GetCurrentProcess(),
            &mut handle,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle returned one valid, separately owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

pub(super) struct Command {
    program: PathBuf,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, Option<OsString>)>,
    directory: Option<PathBuf>,
}

impl Command {
    pub(super) fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: PathBuf::from(program.as_ref()),
            arguments: Vec::new(),
            environment: Vec::new(),
            directory: None,
        }
    }

    pub(super) fn arg(&mut self, argument: impl AsRef<OsStr>) -> &mut Self {
        self.arguments.push(argument.as_ref().to_os_string());
        self
    }

    pub(super) fn args<I, S>(&mut self, arguments: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for argument in arguments {
            self.arg(argument);
        }
        self
    }

    pub(super) fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.environment.push((
            key.as_ref().to_os_string(),
            Some(value.as_ref().to_os_string()),
        ));
        self
    }

    pub(super) fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.environment.push((key.as_ref().to_os_string(), None));
        self
    }

    pub(super) fn current_dir(&mut self, directory: impl AsRef<Path>) -> &mut Self {
        self.directory = Some(directory.as_ref().to_path_buf());
        self
    }

    pub(super) fn spawn_captured(&self, stdout: &File, stderr: &File) -> io::Result<Process> {
        self.spawn_suspended(stdout, stderr)?.resume()
    }

    fn spawn_suspended(&self, stdout: &File, stderr: &File) -> io::Result<SuspendedProcess> {
        validate_program(&self.program)?;
        let application = terminated(self.program.as_os_str())?;
        let mut command_line = command_line(self.program.as_os_str(), &self.arguments)?;
        let environment = environment_block(std::env::vars_os(), &self.environment)?;
        let directory = match &self.directory {
            Some(directory) => {
                if !directory.is_absolute() {
                    return Err(invalid("Verification working directory must be absolute"));
                }
                Some(terminated(directory.as_os_str())?)
            }
            None => None,
        };
        let job = Job::new()?;
        let stdin = File::open("NUL")?;
        let input_handle = inheritable_copy(&stdin)?;
        let output_handle = inheritable_copy(stdout)?;
        let error_handle = inheritable_copy(stderr)?;
        let mut job_handles = [job.0.as_raw_handle()];
        let mut inherited_handles = [
            input_handle.as_raw_handle(),
            output_handle.as_raw_handle(),
            error_handle.as_raw_handle(),
        ];
        // Payload arrays/handles were declared first so the list is destroyed
        // before either payload goes away, including every early error path.
        let mut attributes = Attributes::new(2)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &mut job_handles)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &mut inherited_handles)?;
        // SAFETY: these repr(C) structures contain only integers and raw pointers;
        // zero is valid for every unused/reserved field documented by Win32.
        let mut startup: StartupInfoEx = unsafe { std::mem::zeroed() };
        startup.startup.cb = std::mem::size_of::<StartupInfoEx>() as u32;
        startup.startup.flags = STARTF_USESTDHANDLES;
        startup.startup.stdin = inherited_handles[0];
        startup.startup.stdout = inherited_handles[1];
        startup.startup.stderr = inherited_handles[2];
        startup.attributes = attributes.pointer();
        // SAFETY: PROCESS_INFORMATION is an output-only structure of handles and
        // identifiers, all of which may be zero before CreateProcessW fills it.
        let mut information: ProcessInformation = unsafe { std::mem::zeroed() };
        // SAFETY: executable/cwd/environment are valid terminated UTF-16 buffers,
        // command_line is writable, and STARTUPINFOEX plus both attribute payloads
        // remain alive. Explicit application avoids path-with-spaces ambiguity.
        // Only the three listed stdio handles are inherited; the Job never is.
        if unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_NO_WINDOW
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_SUSPENDED,
                environment.as_ptr().cast(),
                directory
                    .as_ref()
                    .map_or(std::ptr::null(), |directory| directory.as_ptr()),
                &startup.startup,
                &mut information,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful CreateProcessW returns two valid owned handles.
        // Install the terminating Process guard before any fallible operation.
        let process_handle = unsafe { OwnedHandle::from_raw_handle(information.process) };
        // SAFETY: this independently owned primary-thread handle has not been
        // closed/wrapped elsewhere and is closed after resumption or cancellation.
        let thread = unsafe { OwnedHandle::from_raw_handle(information.thread) };
        let suspended = SuspendedProcess {
            process: Some(Process {
                handle: process_handle,
                job: Some(job),
                #[cfg(test)]
                id: information.process_id,
            }),
            thread,
        };
        drop(attributes);
        // These handles are temporarily inheritable. Close their parent copies
        // promptly; HANDLE_LIST restricts our child but cannot coordinate another
        // concurrent std::Command spawn's private inheritance mutex.
        drop((input_handle, output_handle, error_handle));
        if !suspended.is_in_job()? {
            return Err(io::Error::other(
                "Verification process is outside its private job",
            ));
        }
        Ok(suspended)
    }

    #[cfg(test)]
    pub(super) fn spawn_suspended_for_test(
        &self,
        stdout: &File,
        stderr: &File,
    ) -> io::Result<SuspendedProcess> {
        self.spawn_suspended(stdout, stderr)
    }
}

pub(super) struct SuspendedProcess {
    process: Option<Process>,
    thread: OwnedHandle,
}

impl SuspendedProcess {
    fn is_in_job(&self) -> io::Result<bool> {
        let process = self
            .process
            .as_ref()
            .ok_or_else(|| invalid("Verification process has already resumed"))?;
        let job = process
            .job
            .as_ref()
            .ok_or_else(|| invalid("Verification job has already closed"))?;
        let mut associated = 0;
        // SAFETY: both handles are live owned handles, and the output BOOL is
        // writable. Query the exact private job, not an unrelated outer CI job.
        if unsafe {
            IsProcessInJob(
                process.handle.as_raw_handle(),
                job.0.as_raw_handle(),
                &mut associated,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(associated != 0)
    }

    pub(super) fn resume(mut self) -> io::Result<Process> {
        // SAFETY: the primary thread belongs to our guarded suspended process.
        // No other code in this module suspends/resumes that thread.
        let previous = unsafe { ResumeThread(self.thread.as_raw_handle()) };
        if previous == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        if previous != 1 {
            return Err(io::Error::other(
                "Unexpected verification thread suspension count",
            ));
        }
        self.process
            .take()
            .ok_or_else(|| invalid("Verification process has already resumed"))
    }

    #[cfg(test)]
    pub(super) fn is_in_job_for_test(&self) -> io::Result<bool> {
        self.is_in_job()
    }

    #[cfg(test)]
    pub(super) fn id_for_test(&self) -> u32 {
        self.process
            .as_ref()
            .expect("The suspended fixture has not resumed")
            .id()
    }
}

pub(super) struct Process {
    handle: OwnedHandle,
    job: Option<Job>,
    #[cfg(test)]
    id: u32,
}

impl Process {
    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        // SAFETY: the owned process handle remains valid after process exit.
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: the process is signaled/exited and code is writable.
                // An exited process may legitimately return 259, so waiting is
                // essential instead of treating STILL_ACTIVE as a status test.
                if unsafe { GetExitCodeProcess(self.handle.as_raw_handle(), &mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(ExitStatus::from_raw(code)))
            }
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            other => Err(io::Error::other(format!(
                "Unexpected process wait result: {other}"
            ))),
        }
    }

    pub(super) fn terminate_descendants(&self) {
        if let Some(job) = &self.job {
            job.terminate();
        }
    }

    #[cfg(test)]
    pub(super) fn id(&self) -> u32 {
        self.id
    }

    #[cfg(test)]
    pub(super) fn process_handle(&self) -> Handle {
        self.handle.as_raw_handle()
    }

    #[cfg(test)]
    pub(super) fn job_handle(&self) -> Handle {
        self.job
            .as_ref()
            .expect("The fixture still owns its Job")
            .0
            .as_raw_handle()
    }

    #[cfg(test)]
    pub(super) fn close_job_without_terminate_for_test(&mut self) -> io::Result<()> {
        let job = self
            .job
            .take()
            .ok_or_else(|| invalid("Verification job has already closed"))?;
        let job = std::mem::ManuallyDrop::new(job);
        // SAFETY: ManuallyDrop transfers responsibility for closing this sole
        // Job handle here, skipping both explicit termination and OwnedHandle.
        if unsafe { CloseHandle(job.0.as_raw_handle()) } == 0 {
            self.job = Some(std::mem::ManuallyDrop::into_inner(job));
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.terminate_descendants();
        // SAFETY: explicit fallback termination and a finite wait use only this
        // owned process handle, never a possibly reused PID. OwnedHandle and Job
        // still close on every path, including errors or a delayed OS teardown.
        unsafe {
            TerminateProcess(self.handle.as_raw_handle(), 1);
            WaitForSingleObject(self.handle.as_raw_handle(), REAP_TIMEOUT_MS);
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn terminated(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(invalid("Verification command contains a null character"));
    }
    value.push(0);
    Ok(value)
}

fn validate_program(program: &Path) -> io::Result<()> {
    if !program.is_absolute()
        || !program
            .extension()
            .is_some_and(|extension| extension.as_encoded_bytes().eq_ignore_ascii_case(b"exe"))
    {
        return Err(invalid(
            "Verification program must be an absolute .exe path",
        ));
    }
    Ok(())
}

fn quoted_argument(argument: &OsStr) -> io::Result<Vec<u16>> {
    let mut quoted = vec![b'"' as u16];
    let mut backslashes = 0;
    for unit in argument.encode_wide() {
        match unit {
            0 => return Err(invalid("Verification argument contains a null character")),
            92 => backslashes += 1,
            34 => {
                quoted.extend(std::iter::repeat_n(92, backslashes * 2 + 1));
                quoted.push(unit);
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n(92, backslashes));
                quoted.push(unit);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n(92, backslashes * 2));
    quoted.push(b'"' as u16);
    Ok(quoted)
}

fn command_line(program: &OsStr, arguments: &[OsString]) -> io::Result<Vec<u16>> {
    let program: Vec<u16> = program.encode_wide().collect();
    if program.contains(&0) || program.contains(&(b'"' as u16)) {
        return Err(invalid(
            "Verification program path contains an invalid character",
        ));
    }
    // argv[0] has special CRT parsing rules: quote its path without applying the
    // backslash escapes used for subsequent ordinary arguments.
    let mut line = vec![b'"' as u16];
    line.extend(program);
    line.push(b'"' as u16);
    for argument in arguments {
        line.push(b' ' as u16);
        line.extend(quoted_argument(argument)?);
        if line.len() >= MAX_COMMAND_LINE_UNITS {
            return Err(invalid("Verification command line is too long"));
        }
    }
    line.push(0);
    if line.len() > MAX_COMMAND_LINE_UNITS {
        return Err(invalid("Verification command line is too long"));
    }
    Ok(line)
}

struct EnvironmentEntry {
    key: Vec<u16>,
    value: Vec<u16>,
}

fn environment_key(key: &OsStr, inherited: bool) -> io::Result<Vec<u16>> {
    let key: Vec<u16> = key.encode_wide().collect();
    if key.is_empty()
        || key.contains(&0)
        || key.len() > i32::MAX as usize
        || key
            .iter()
            .enumerate()
            .any(|(index, unit)| *unit == 61 && !(inherited && index == 0))
    {
        return Err(invalid(
            "Verification environment has an invalid variable name",
        ));
    }
    Ok(key)
}

fn compare_keys(left: &[u16], right: &[u16]) -> io::Result<Ordering> {
    // SAFETY: both buffers are valid UTF-16/WTF-16 OS strings with lengths
    // validated to fit i32. Windows' ordinal uppercase table, not Rust Unicode
    // case conversion, defines environment-name equality and ordering.
    match unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    } {
        1 => Ok(Ordering::Less),
        2 => Ok(Ordering::Equal),
        3 => Ok(Ordering::Greater),
        _ => Err(io::Error::last_os_error()),
    }
}

fn environment_change(
    entries: &mut Vec<EnvironmentEntry>,
    key: &OsStr,
    value: Option<&OsStr>,
    inherited: bool,
) -> io::Result<()> {
    let key = environment_key(key, inherited)?;
    let value = value
        .map(|value| {
            let value: Vec<u16> = value.encode_wide().collect();
            if value.contains(&0) {
                Err(invalid(
                    "Verification environment value contains a null character",
                ))
            } else {
                Ok(value)
            }
        })
        .transpose()?;
    // Keep entries ordered with a fallible binary search so an unexpected API
    // comparison failure returns an error rather than panicking inside sort.
    let mut low = 0;
    let mut high = entries.len();
    while low < high {
        let middle = low + (high - low) / 2;
        match compare_keys(&entries[middle].key, &key)? {
            Ordering::Less => low = middle + 1,
            Ordering::Greater => high = middle,
            Ordering::Equal => {
                if let Some(value) = value {
                    entries[middle] = EnvironmentEntry { key, value };
                } else {
                    entries.remove(middle);
                }
                return Ok(());
            }
        }
    }
    if let Some(value) = value {
        entries.insert(low, EnvironmentEntry { key, value });
    }
    Ok(())
}

fn environment_block(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    changes: &[(OsString, Option<OsString>)],
) -> io::Result<Vec<u16>> {
    let mut entries = Vec::new();
    for (key, value) in inherited {
        // vars_os preserves Windows' special =C: current-drive entries. They
        // must be retained when inheriting, but cannot be supplied as overrides.
        environment_change(&mut entries, &key, Some(&value), true)?;
    }
    for (key, value) in changes {
        environment_change(&mut entries, key, value.as_deref(), false)?;
    }
    let mut block = Vec::new();
    for entry in entries {
        block.extend(entry.key);
        block.push(b'=' as u16);
        block.extend(entry.value);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStringExt;

    fn quote(argument: &str) -> String {
        String::from_utf16(&quoted_argument(OsStr::new(argument)).unwrap()).unwrap()
    }

    #[test]
    fn crt_quoting_preserves_empty_space_quotes_and_trailing_backslashes() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("a b\tc"), "\"a b\tc\"");
        assert_eq!(quote("Mauro ñ 空"), "\"Mauro ñ 空\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(
            quote("C:\\path with spaces\\"),
            "\"C:\\path with spaces\\\\\""
        );
        assert_eq!(quote("C:\\plain\\file"), "\"C:\\plain\\file\"");
    }

    #[test]
    fn command_line_quotes_executable_separately_and_preserves_wtf16() {
        let program = OsStr::new(r"C:\Program Files\TerminalCanvas\mi-terminal.exe");
        let arguments = [OsString::from(""), OsString::from("--flag")];
        let line = command_line(program, &arguments).unwrap();
        assert_eq!(*line.last().unwrap(), 0);
        assert_eq!(
            String::from_utf16(&line[..line.len() - 1]).unwrap(),
            "\"C:\\Program Files\\TerminalCanvas\\mi-terminal.exe\" \"\" \"--flag\""
        );
        let unpaired = OsString::from_wide(&[0xd800, 92, 34]);
        assert_eq!(
            quoted_argument(&unpaired).unwrap(),
            [34, 0xd800, 92, 92, 92, 34, 34]
        );
    }

    #[test]
    fn invalid_programs_arguments_and_oversized_command_lines_are_rejected() {
        for program in [
            "powershell.exe",
            r"C:\tools\helper.cmd",
            r"\tools\helper.exe",
        ] {
            assert!(validate_program(Path::new(program)).is_err());
        }
        assert!(validate_program(Path::new(r"C:\tools\helper.EXE")).is_ok());
        assert!(command_line(OsStr::new("C:\\bad\"name.exe"), &[]).is_err());
        assert!(command_line(OsStr::new("C:\\bad\0name.exe"), &[]).is_err());
        assert!(quoted_argument(OsStr::new("bad\0arg")).is_err());
        assert!(command_line(
            OsStr::new(r"C:\helper.exe"),
            &[OsString::from("x".repeat(MAX_COMMAND_LINE_UNITS))]
        )
        .is_err());
    }

    #[test]
    fn environment_merges_unicode_names_removes_case_insensitively_and_preserves_drive_entries() {
        let inherited = [
            (OsString::from("SystemRoot"), OsString::from(r"C:\Windows")),
            (OsString::from("=C:"), OsString::from(r"C:\Projects")),
            (OsString::from("PSModulePath"), OsString::from("untrusted")),
            (OsString::from("CAMINO_Ñ"), OsString::from("old")),
        ];
        let changes = [
            (OsString::from("psmodulepath"), None),
            (
                OsString::from("camino_ñ"),
                Some(OsString::from("Mauro ñ 空")),
            ),
        ];
        let block = environment_block(inherited, &changes).unwrap();
        assert_eq!(
            String::from_utf16(&block).unwrap(),
            "=C:=C:\\Projects\0camino_ñ=Mauro ñ 空\0SystemRoot=C:\\Windows\0\0"
        );
        assert_eq!(environment_block([], &[]).unwrap(), [0, 0]);
    }

    #[test]
    fn later_environment_changes_win_and_values_keep_equals_and_wtf16() {
        let inherited = [(OsString::from("TC_TEST"), OsString::from("initial"))];
        let changes = [
            (OsString::from("tc_test"), None),
            (
                OsString::from("TC_TEST"),
                Some(OsString::from("after=remove")),
            ),
            (
                OsString::from("tc_test"),
                Some(OsString::from_wide(&[0xd800, 61])),
            ),
        ];
        let block = environment_block(inherited, &changes).unwrap();
        let expected: Vec<u16> = "tc_test="
            .encode_utf16()
            .chain([0xd800, 61, 0, 0])
            .collect();
        assert_eq!(block, expected);
    }

    #[test]
    fn invalid_environment_names_and_null_values_are_rejected() {
        for key in ["", "a=b", "=C:", "a\0b"] {
            let changes = [(OsString::from(key), Some(OsString::from("value")))];
            assert!(environment_block([], &changes).is_err());
            let removed = [(OsString::from(key), None)];
            assert!(environment_block([], &removed).is_err());
        }
        assert!(environment_block(
            [],
            &[(OsString::from("VALID"), Some(OsString::from("bad\0value")))]
        )
        .is_err());
    }

    #[test]
    fn win32_structures_match_the_native_abi() {
        if cfg!(target_pointer_width = "64") {
            assert_eq!(std::mem::size_of::<StartupInfo>(), 104);
            assert_eq!(std::mem::size_of::<StartupInfoEx>(), 112);
            assert_eq!(std::mem::size_of::<ProcessInformation>(), 24);
            assert_eq!(std::mem::size_of::<BasicLimitInformation>(), 64);
            assert_eq!(std::mem::size_of::<ExtendedLimitInformation>(), 144);
            assert_eq!(std::mem::offset_of!(StartupInfo, stdin), 80);
        } else {
            assert_eq!(std::mem::size_of::<StartupInfo>(), 68);
            assert_eq!(std::mem::size_of::<StartupInfoEx>(), 72);
            assert_eq!(std::mem::size_of::<ProcessInformation>(), 16);
            assert_eq!(std::mem::size_of::<BasicLimitInformation>(), 48);
            assert_eq!(std::mem::size_of::<ExtendedLimitInformation>(), 112);
            assert_eq!(std::mem::offset_of!(StartupInfo, stdin), 56);
        }
        assert_eq!(std::mem::offset_of!(StartupInfoEx, startup), 0);
        assert_eq!(
            std::mem::offset_of!(StartupInfoEx, attributes),
            std::mem::size_of::<StartupInfo>()
        );
    }
}
