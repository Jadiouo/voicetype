//! A private kill-on-close Job Object for any Windows child process tree.
use std::{
    io,
    mem::size_of,
    os::windows::io::{AsRawHandle, RawHandle},
    process::Child,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

pub struct WindowsJob(HANDLE);
// A kernel handle is process-wide and owned exactly once by this RAII wrapper.
unsafe impl Send for WindowsJob {}

impl WindowsJob {
    /// Must be called immediately after spawn, before any child protocol work.
    /// Failure means the caller must kill and reap the child before returning.
    pub fn attach(child: &Child) -> io::Result<Self> {
        // SAFETY: Child owns this live process handle for the duration of call.
        unsafe { Self::attach_raw_handle(child.as_raw_handle()) }
    }

    /// Attach a process handle owned by another process abstraction (e.g. a
    /// ConPTY child). The handle is borrowed, not closed by this function.
    ///
    /// # Safety
    /// `process` must be a live, valid process HANDLE throughout this call.
    pub unsafe fn attach_raw_handle(process: RawHandle) -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { AssignProcessToJobObject(handle, process as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
