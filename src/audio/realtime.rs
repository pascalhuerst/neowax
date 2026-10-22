use libc::{
    cpu_set_t, sched_param, sched_setaffinity, sched_setscheduler, CPU_SET, CPU_ZERO, SCHED_FIFO,
};

/// Id that makes the scheduler calls below act on the calling thread. Not
/// `getpid()`: that is the id of the *main* thread, so a call made from an
/// audio thread would reschedule the UI thread instead.
const CALLING_THREAD: libc::pid_t = 0;

/// Set the calling thread's scheduling policy to SCHED_FIFO with priority 80
/// (matching legacy xwax DEFAULT_PRIORITY).
pub fn prioritize_thread() {
    unsafe {
        let param = sched_param { sched_priority: 80 };

        if sched_setscheduler(CALLING_THREAD, SCHED_FIFO, &param) != 0 {
            tracing::warn!(
                "Could not set thread priority - consider running: sudo setcap 'cap_sys_nice=eip' <application>"
            );
        }
    }
}

/// Pin the calling thread to a specific CPU core (e.g., core 0).
pub fn set_thread_affinity(core_id: usize) {
    unsafe {
        let mut set: cpu_set_t = std::mem::zeroed();
        CPU_ZERO(&mut set);
        CPU_SET(core_id, &mut set);

        if sched_setaffinity(CALLING_THREAD, std::mem::size_of::<cpu_set_t>(), &set) != 0 {
            tracing::warn!("Could not set thread affinity");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Number of CPUs the calling thread may run on.
    fn allowed_cpus() -> i32 {
        unsafe {
            let mut set: cpu_set_t = std::mem::zeroed();
            libc::sched_getaffinity(0, std::mem::size_of::<cpu_set_t>(), &mut set);
            libc::CPU_COUNT(&set)
        }
    }

    /// First CPU the calling thread may run on.
    fn first_allowed_cpu() -> usize {
        unsafe {
            let mut set: cpu_set_t = std::mem::zeroed();
            libc::sched_getaffinity(0, std::mem::size_of::<cpu_set_t>(), &mut set);
            (0..libc::CPU_SETSIZE as usize)
                .find(|&c| libc::CPU_ISSET(c, &set))
                .unwrap_or(0)
        }
    }

    #[test]
    fn affinity_applies_to_the_calling_thread_only() {
        let before = allowed_cpus();
        let core = first_allowed_cpu();
        let pinned = std::thread::spawn(move || {
            set_thread_affinity(core);
            allowed_cpus()
        })
        .join()
        .unwrap();

        assert_eq!(pinned, 1, "the thread that asked must be pinned");
        // A thread started earlier (here: this one) must keep its CPUs
        assert_eq!(allowed_cpus(), before, "other threads must be left alone");
    }
}
