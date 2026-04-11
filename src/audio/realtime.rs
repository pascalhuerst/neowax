use libc::{
    cpu_set_t, getpid, sched_param, sched_setaffinity, sched_setscheduler, CPU_SET, CPU_ZERO,
    SCHED_FIFO,
};

/// Set the calling thread's scheduling policy to SCHED_FIFO with priority 80
/// (matching legacy xwax DEFAULT_PRIORITY).
pub fn prioritize_thread() {
    unsafe {
        let param = sched_param { sched_priority: 80 };
        let pid = getpid();

        if sched_setscheduler(pid, SCHED_FIFO, &param) != 0 {
            eprintln!(
                "Could not set thread priority - consider running:\n  sudo setcap 'cap_sys_nice=eip' <application>"
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

        let pid = getpid();

        if sched_setaffinity(pid, std::mem::size_of::<cpu_set_t>(), &set) != 0 {
            eprintln!("Could not set thread affinity");
        }
    }
}
