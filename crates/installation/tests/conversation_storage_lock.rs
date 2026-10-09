use cairn_installation::conversation_lifecycle::storage_lock;

#[test]
fn completed_storage_operation_releases_lock_even_while_a_child_is_starting() {
    let directory = tempfile::tempdir().unwrap();
    let lock = storage_lock(directory.path()).unwrap();
    assert!(storage_lock(directory.path()).is_err());
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        // Only async-signal-safe operations after fork. Simulate a child that
        // inherited descriptors and has not reached exec/CLOEXEC yet.
        unsafe {
            libc::close(pipe[1]);
            let mut byte = 0_u8;
            libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1);
            libc::_exit(0);
        }
    }
    unsafe { libc::close(pipe[0]) };
    drop(lock);
    let next = storage_lock(directory.path());
    unsafe {
        let byte = 1_u8;
        libc::write(pipe[1], (&byte as *const u8).cast(), 1);
        libc::close(pipe[1]);
        libc::waitpid(child, std::ptr::null_mut(), 0);
    }
    assert!(
        next.is_ok(),
        "completed operation must release its lock before the child execs"
    );
}
