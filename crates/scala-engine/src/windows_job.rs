//! Owned, non-inheritable kill-on-close job. Attach while the child is suspended,
//! before it can create workers; fail closed if assignment/resumption fails.
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

pub(super) struct OwnedJob(OwnedHandle);

impl OwnedJob {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: null security attributes create a non-inheritable unnamed job.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this is a new unique kernel handle owned by this guard.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: initialized structure remains alive through the synchronous call.
        if unsafe {
            SetInformationJobObject(
                job.handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn handle(&self) -> HANDLE {
        self.0.as_raw_handle()
    }

    pub(super) fn attach_and_resume(&self, child: &tokio::process::Child) -> io::Result<()> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("Missing suspended child PID"))?;
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("Missing child handle"))?;
        // SAFETY: the supervisor still owns this suspended child and the job.
        if unsafe { AssignProcessToJobObject(self.handle(), process as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // std/tokio do not retain the primary thread handle. A suspended newly
        // created process has one thread; find only that owned child's thread.
        // SAFETY: the snapshot is read-only and contains no borrowed pointers.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: snapshot is a fresh valid uniquely owned handle.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut thread_id = None;
        // SAFETY: structure has the API-required size and remains live.
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                if thread_id.replace(entry.th32ThreadID).is_some() {
                    return Err(io::Error::other("Suspended child has multiple threads"));
                }
            }
            // SAFETY: same snapshot and initialized output structure.
            found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        let tid = thread_id.ok_or_else(|| io::Error::other("Missing suspended primary thread"))?;
        // SAFETY: the PID cannot be reused while our child process handle lives.
        let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) };
        if thread.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: thread is a fresh unique handle; resume only the owned primary.
        let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
        let count = unsafe { ResumeThread(thread.as_raw_handle()) };
        if count != 1 {
            return Err(io::Error::other("Could not resume owned primary thread"));
        }
        Ok(())
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        // SAFETY: only processes in this private owned job are terminated.
        if unsafe { TerminateJobObject(self.handle(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
