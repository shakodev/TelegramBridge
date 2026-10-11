//! Windows job ownership closes the child process tree if the bridge dies.
#[cfg(windows)]
pub struct ProcessGuard(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
unsafe impl Send for ProcessGuard {}
#[cfg(windows)]
impl ProcessGuard {
    pub fn attach(child: &tokio::process::Child) -> anyhow::Result<Self> {
        use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        unsafe {
            let process = child
                .raw_handle()
                .ok_or_else(|| anyhow::anyhow!("Missing child process handle"))?;
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            );
            let assigned = configured != 0 && AssignProcessToJobObject(handle, process.cast()) != 0;
            if !assigned {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(error.into());
            }
            Ok(Self(handle))
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(not(windows))]
pub struct ProcessGuard;
#[cfg(not(windows))]
impl ProcessGuard {
    pub fn attach(_: &tokio::process::Child) -> anyhow::Result<Self> {
        Ok(Self)
    }
}
