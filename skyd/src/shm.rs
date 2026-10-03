// One fresh POSIX shared-memory object per frame, as intermission's engine
// does. The terminal unlinks each object once it reads it; one it never read
// (pane hidden, frame folded) would leak, so the oldest are unlinked here once
// FRAMES_IN_FLIGHT newer ones exist, and all of ours on exit.

use std::ffi::CString;

const FRAMES_IN_FLIGHT: u64 = 8;

pub struct FrameWriter {
    prefix: String,
    next: u64,
}

impl FrameWriter {
    // macOS caps shm names at 30 bytes, slash included
    pub fn new(prefix: String) -> Self {
        Self { prefix, next: 0 }
    }

    fn name(&self, n: u64) -> CString {
        CString::new(format!("{}{n}", self.prefix)).expect("no NUL in prefix")
    }

    pub fn write(&mut self, rgb: &[u8], width: usize, height: usize) -> Result<String, String> {
        let size = width * height * 3;
        debug_assert_eq!(rgb.len(), size);
        if self.next >= FRAMES_IN_FLIGHT {
            unsafe { libc::shm_unlink(self.name(self.next - FRAMES_IN_FLIGHT).as_ptr()) };
        }
        let name = self.name(self.next);
        self.next += 1;

        unsafe {
            let fd = libc::shm_open(name.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600);
            if fd < 0 {
                return Err(format!("shm_open: {}", std::io::Error::last_os_error()));
            }
            if libc::ftruncate(fd, size as libc::off_t) != 0 {
                let error = std::io::Error::last_os_error();
                libc::close(fd);
                libc::shm_unlink(name.as_ptr());
                return Err(format!("ftruncate: {error}"));
            }
            let out = libc::mmap(std::ptr::null_mut(), size, libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            libc::close(fd);
            if out == libc::MAP_FAILED {
                libc::shm_unlink(name.as_ptr());
                return Err(format!("mmap: {}", std::io::Error::last_os_error()));
            }
            std::ptr::copy_nonoverlapping(rgb.as_ptr(), out as *mut u8, size);
            libc::munmap(out, size);
        }
        Ok(name.into_string().unwrap())
    }

    pub fn cleanup(&mut self) {
        let first = self.next.saturating_sub(FRAMES_IN_FLIGHT);
        for n in first..self.next {
            unsafe { libc::shm_unlink(self.name(n).as_ptr()) };
        }
    }
}
