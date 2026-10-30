//! Starting the signer's `gh` and `gcloud`, retrying while Linux reports them busy.

use std::io;
use std::thread;
use std::time::Duration;

/// Retries `attempt` briefly while starting the program fails with ETXTBSY.
///
/// Linux won't exec a file that any process has open for writing, and a program written a
/// moment ago can still be open in a child another thread forked mid-write. Parallel tests
/// that write a fake `gh` or `gcloud` hit this, as could a just-updated install.
pub fn spawn_with_etxtbsy_retry<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    const ATTEMPTS: u32 = 5;
    let mut tries = 1;
    loop {
        match attempt() {
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy && tries < ATTEMPTS => {
                thread::sleep(Duration::from_millis(u64::from(20 * tries)));
                tries += 1;
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Error;

    use super::*;

    const ETXTBSY: i32 = 26;
    const ENOENT: i32 = 2;

    #[test]
    fn retries_a_busy_program_until_it_starts() {
        let calls = Cell::new(0);
        let started = spawn_with_etxtbsy_retry(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(Error::from_raw_os_error(ETXTBSY))
            } else {
                Ok("started")
            }
        });
        assert_eq!(started.unwrap(), "started");
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn gives_up_after_five_busy_attempts() {
        let calls = Cell::new(0);
        let err = spawn_with_etxtbsy_retry(|| -> io::Result<()> {
            calls.set(calls.get() + 1);
            Err(Error::from_raw_os_error(ETXTBSY))
        })
        .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(ETXTBSY));
        assert_eq!(calls.get(), 5);
    }

    #[test]
    fn returns_any_other_error_at_once() {
        let calls = Cell::new(0);
        let err = spawn_with_etxtbsy_retry(|| -> io::Result<()> {
            calls.set(calls.get() + 1);
            Err(Error::from_raw_os_error(ENOENT))
        })
        .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(ENOENT));
        assert_eq!(calls.get(), 1);
    }
}
