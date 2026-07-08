use std::time::Duration;
use tokio::time::sleep;

/// Verify that `process_group(0)` isolates a spawned `sh -c` into its
/// own process group AND that `killpg` can kill the entire group.
#[tokio::test]
async fn test_process_group_killpg_terminates_tree() {
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 120")
        .process_group(0)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sh -c sleep 120");

    let pid = child.id().expect("child should have a PID");

    // The child should have its own PGID equal to its PID
    // (process_group(0) calls setpgid(0, 0) in the child)
    let child_pgid = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(pid as i32)))
        .expect("child should have a process group");
    assert_eq!(
        child_pgid,
        nix::unistd::Pid::from_raw(pid as i32),
        "PGID should match PID (new process group)"
    );

    // Kill the entire process group
    nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .expect("killpg should succeed");

    // Reap the child — wait() should return quickly with a signal status
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("child.wait() should complete within 5s")
        .expect("child.wait() should succeed");

    assert!(
        !status.success(),
        "child should have been killed by signal (status={:?})",
        status.code()
    );
}

/// Verify that the oneshot-channel cancel pattern used by the bot actually
/// breaks out of a select! loop even when the output channel is active.
#[tokio::test]
async fn test_oneshot_cancel_breaks_select_loop() {
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<String>(16);

    let select_task = tokio::spawn(async move {
        tokio::pin!(cancel_rx);

        let feeder = tokio::spawn(async move {
            for i in 0..10 {
                let _ = output_tx.send(format!("chunk_{i}")).await;
                sleep(Duration::from_millis(1)).await;
            }
        });

        let result = loop {
            tokio::select! {
                Some(_chunk) = output_rx.recv() => {
                    sleep(Duration::from_millis(5)).await;
                }
                _ = &mut cancel_rx => {
                    break true;
                }
            }
        };

        feeder.await.ok();
        result
    });

    // Let the loop start before sending the signal
    sleep(Duration::from_millis(30)).await;

    cancel_tx.send(()).expect("cancel_tx.send should succeed");

    let result: bool = tokio::time::timeout(Duration::from_secs(10), select_task)
        .await
        .expect("select loop should exit within 10 seconds")
        .expect("select task should not panic");

    assert!(result, "cancelled should be true after cancel signal");
}
