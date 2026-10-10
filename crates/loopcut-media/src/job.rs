//! Process containment with a Windows Job Object.
//!
//! Every FFmpeg or ffprobe child LoopCut starts is assigned to one process-wide job object created with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. When LoopCut exits or crashes, the operating system closes the last handle to
//! that job and terminates every child in it, so no orphaned encoder keeps running and holds DLLs open (which would
//! block an update). On other platforms [`assign`] and [`terminate_all`] are no-ops, keeping the crate portable.

use std::io;
use std::process::Child;

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::ptr;

    use parking_lot::Mutex;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows::core::PCWSTR;

    /// Size passed to `SetInformationJobObject`; a fixed struct, so the conversion cannot truncate.
    const LIMIT_INFO_SIZE: u32 = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;

    /// Owned job handle wrapper. The handle is closed only by [`terminate_all`], which replaces the job.
    struct JobHandle(HANDLE);

    // SAFETY: a job object handle is a kernel object reference, not a thread-affine resource. The only operations
    // performed on it (`AssignProcessToJobObject`, `TerminateJobObject`, `CloseHandle`) are thread-safe kernel calls,
    // and every access goes through the `JOB` mutex, so the handle is never used after it has been closed.
    unsafe impl Send for JobHandle {}

    /// The process-wide job. `None` until the first child is assigned, and again between [`terminate_all`] and the
    /// next assignment when no replacement could be created. Holding the lock across an assignment or a termination
    /// keeps the handle from being closed while another thread uses it.
    static JOB: Mutex<Option<JobHandle>> = Mutex::new(None);

    /// Returns the current job, creating it when there is none.
    fn current_job(slot: &mut Option<JobHandle>) -> io::Result<HANDLE> {
        if let Some(handle) = slot {
            return Ok(handle.0);
        }
        let handle = create_kill_on_close_job()?;
        *slot = Some(JobHandle(handle));
        Ok(handle)
    }

    /// Creates an anonymous job object whose processes are terminated when its last handle closes.
    fn create_kill_on_close_job() -> io::Result<HANDLE> {
        // SAFETY: null security attributes and a null name create an anonymous job owned by this process.
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(to_io)?;

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        // SAFETY: `info` is a fully initialized `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` that outlives the call, and
        // `LIMIT_INFO_SIZE` is exactly its size, as the API requires for this information class.
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&info).cast(),
                LIMIT_INFO_SIZE,
            )
        };
        if let Err(err) = configured {
            // SAFETY: `job` was returned by CreateJobObjectW above and is not used again after this point.
            let _ = unsafe { CloseHandle(job) };
            return Err(to_io(err));
        }
        Ok(job)
    }

    /// Assigns a running child to the job. See [`super::assign`].
    pub fn assign(child: &Child) -> io::Result<()> {
        let mut slot = JOB.lock();
        let job = current_job(&mut slot)?;
        let process = HANDLE(child.as_raw_handle());
        // SAFETY: `job` is a live job handle owned by `JOB` (the lock is held, so it is not closed meanwhile). `process`
        // is the handle of `child`, which stays alive and borrowed for the duration of this call.
        unsafe { AssignProcessToJobObject(job, process) }.map_err(to_io)
    }

    /// Terminates every child in the job and replaces the job with a fresh one. See [`super::terminate_all`].
    pub fn terminate_all() -> io::Result<()> {
        let mut slot = JOB.lock();
        let Some(old) = slot.take() else {
            // No child was ever assigned, so there is nothing to terminate.
            return Ok(());
        };
        // SAFETY: `old.0` is the live job handle taken out of the slot, so no other thread can reach it. Exit code 1
        // marks the children as killed by LoopCut.
        let terminated = unsafe { TerminateJobObject(old.0, 1) }.map_err(to_io);
        // SAFETY: `old.0` is still open (taken from the slot above) and is not used again. Closing the last handle to a
        // kill-on-close job also ends any child that outlived the termination above.
        let _ = unsafe { CloseHandle(old.0) };
        // A replacement for future children. If it cannot be created the slot stays empty, and the next `assign`
        // retries the creation and reports its error.
        if let Ok(fresh) = create_kill_on_close_job() {
            *slot = Some(JobHandle(fresh));
        }
        terminated
    }

    /// Converts a `windows` error into an `io::Error`, keeping the Win32 code when there is one.
    fn to_io(err: windows::core::Error) -> io::Error {
        let code = err.code().0 as u32;
        // HRESULT_FROM_WIN32 wraps a Win32 error code in the low 16 bits under the 0x8007 facility.
        if code & 0xFFFF_0000 == 0x8007_0000 {
            io::Error::from_raw_os_error((code & 0xFFFF) as i32)
        } else {
            io::Error::other(err.to_string())
        }
    }
}

/// Assigns a freshly spawned child process to LoopCut's kill-on-close Job Object.
///
/// Call this right after spawning. On Windows this creates the job on first use and assigns `child`; elsewhere it
/// does nothing. A failure leaves the child running uncontained, so the runner treats it as fatal unless the child
/// has already exited.
#[cfg(windows)]
pub fn assign(child: &Child) -> io::Result<()> {
    imp::assign(child)
}

/// No-op outside Windows; the crate keeps the same API everywhere.
#[cfg(not(windows))]
pub fn assign(_child: &Child) -> io::Result<()> {
    Ok(())
}

/// Terminates every child currently in LoopCut's Job Object, then replaces the job so later children are contained
/// again.
///
/// Used by the update path when cancelled jobs do not exit in time, so the installer can replace locked FFmpeg files.
/// Children that were already gone are not an error. Elsewhere this is a no-op.
///
/// # Errors
///
/// The Win32 error when `TerminateJobObject` fails. The job is still replaced in that case.
#[cfg(windows)]
pub fn terminate_all() -> io::Result<()> {
    imp::terminate_all()
}

/// No-op outside Windows; the crate keeps the same API everywhere.
#[cfg(not(windows))]
pub fn terminate_all() -> io::Result<()> {
    Ok(())
}
