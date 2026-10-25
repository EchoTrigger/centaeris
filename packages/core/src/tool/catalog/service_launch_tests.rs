//! Execute the service-launch example actually supplied to the model.
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::list_tool_contracts;

#[test]
fn bash_service_example_releases_tool_pipes_while_service_is_alive() {
    let bash = list_tool_contracts()
        .into_iter()
        .find(|contract| contract.name == "bash")
        .expect("bash contract");
    let example = bash
        .summary
        .split_once("```bash\n")
        .and_then(|(_, rest)| rest.split_once("```"))
        .map(|(script, _)| script.trim())
        .expect("the service guidance must include an executable Bash example");
    let outcome = service_launch_outcome(example);
    assert!(outcome.shell_exited, "the foreground shell must finish");
    assert!(
        outcome.output_closed,
        "the service must not retain tool output pipes"
    );
    assert!(
        outcome.service_alive,
        "the service must survive the foreground command"
    );
}

#[test]
fn service_probe_detects_inherited_pipes_from_a_background_compound_command() {
    let outcome = service_launch_outcome(
        "cd \"$WORKSPACE\" && nohup \"$SERVICE\" </dev/null >\"$SERVICE_LOG\" 2>&1 &",
    );
    assert!(outcome.shell_exited);
    assert!(outcome.service_alive);
    assert!(
        !outcome.output_closed,
        "the probe must detect the original pipe leak"
    );
}

struct LaunchOutcome {
    shell_exited: bool,
    output_closed: bool,
    service_alive: bool,
}

fn service_launch_outcome(script: &str) -> LaunchOutcome {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "centaeris service example {} {nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&root).expect("isolated service fixture");
    let service = root.join("service.sh");
    let ready = root.join("ready");
    let stop = root.join("stop");
    let done = root.join("done");
    std::fs::write(
        &service,
        "#!/bin/bash\nprintf ready >\"$SERVICE_READY\"\nfor ((i=0; i<200; i++)); do\n  test -f \"$SERVICE_STOP\" && break\n  sleep 0.05\ndone\nprintf done >\"$SERVICE_DONE\"\n",
    )
    .expect("write finite service");
    std::fs::set_permissions(&service, std::fs::Permissions::from_mode(0o700))
        .expect("executable service");
    let mut shell = Command::new("/bin/bash")
        .args(["-c", script])
        .current_dir(&root)
        .env("WORKSPACE", &root)
        .env("SERVICE", &service)
        .env("SERVICE_LOG", root.join("service.log"))
        .env("SERVICE_READY", &ready)
        .env("SERVICE_STOP", &stop)
        .env("SERVICE_DONE", &done)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch model-visible example");
    let mut stdout = shell.stdout.take().expect("stdout pipe");
    let mut stderr = shell.stderr.take().expect("stderr pipe");
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .read_to_end(&mut output)
            .and_then(|_| stderr.read_to_end(&mut output));
        let _ = sender.send(result);
    });
    let output_closed = matches!(receiver.recv_timeout(Duration::from_secs(2)), Ok(Ok(_)));
    let shell_exited = shell
        .try_wait()
        .expect("observe shell")
        .is_some_and(|s| s.success());
    // The service may be scheduled just after the foreground shell exits.
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let service_alive = ready.exists() && !done.exists();
    std::fs::write(&stop, b"stop").expect("stop only our fixture service");
    shell.wait().expect("reap foreground shell");
    reader.join().expect("drain fixture pipes");
    for _ in 0..100 {
        if done.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(done.exists(), "finite fixture service acknowledged stop");
    std::fs::remove_dir_all(root).expect("remove isolated fixture");
    LaunchOutcome {
        shell_exited,
        output_closed,
        service_alive,
    }
}
