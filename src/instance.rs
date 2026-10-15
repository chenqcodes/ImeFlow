//! Atomic, session-local single-instance ownership. Must precede daemon log creation.
use std::marker::PhantomData;
use std::rc::Rc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

pub struct InstanceLock {
    handle: HANDLE,
    _thread_bound: PhantomData<Rc<()>>,
}
impl InstanceLock {
    pub fn acquire() -> Result<Self, String> {
        Self::acquire_named(w!("Local\\ImeFlow"))
    }
    fn acquire_named(name: PCWSTR) -> Result<Self, String> {
        let handle = unsafe { CreateMutexW(None, false, name) }
            .map_err(|e| format!("single-instance mutex: {e}"))?;
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self {
                handle,
                _thread_bound: PhantomData,
            }),
            result => {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                Err(if result == WAIT_TIMEOUT {
                    "Another ImeFlow instance is running".into()
                } else {
                    format!("single-instance mutex wait failed: {}", result.0)
                })
            }
        }
    }
}
impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_owner_is_rejected_and_drop_allows_restart() {
        let name = format!("Local\\ImeFlow-test-{}\0", std::process::id())
            .encode_utf16()
            .collect::<Vec<_>>();
        let owner = InstanceLock::acquire_named(PCWSTR(name.as_ptr())).unwrap();
        let thread_name = name.clone();
        assert!(
            std::thread::spawn(
                move || InstanceLock::acquire_named(PCWSTR(thread_name.as_ptr())).is_err()
            )
            .join()
            .unwrap()
        );
        drop(owner);
        assert!(InstanceLock::acquire_named(PCWSTR(name.as_ptr())).is_ok());
    }
}
