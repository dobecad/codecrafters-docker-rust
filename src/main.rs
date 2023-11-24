use anyhow::{Context, Result};
use std::fs::copy;
use std::os::unix::fs;
use std::process::Stdio;

const CHROOT_DIR: &'static str = "/tmp/codecrafters";

// Usage: your_docker.sh run <image> <command> <arg1> <arg2> ...
fn main() -> Result<()> {
    // You can use print statements as follows for debugging, they'll be visible when running tests.
    // println!("Logs from your program will appear here!");

    // Create the chroot directory and the necessary child directories
    let _ = std::fs::create_dir_all(CHROOT_DIR).context("failed to create chroot directory")?;
    let _ = std::fs::create_dir_all(format!("{}/usr/local/bin", CHROOT_DIR))
        .context("failed to create chroot /usr/local/bin directory")?;
    let _ = std::fs::create_dir_all(format!("{}/dev/null", CHROOT_DIR))
        .context("failed to create chroot /dev/null directory")?;

    // Copy the docker-explorer binary to the chroot /usr/local/bin directory
    let _ = copy(
        "/usr/local/bin/docker-explorer",
        format!("{}/usr/local/bin/docker-explorer", CHROOT_DIR),
    )
    .context("failed to copy docker-explorer")?;

    // Change root directory of current process to our chroot directory we just created
    let _ = fs::chroot(CHROOT_DIR).context("failed to chroot")?;

    unsafe {
        libc::unshare(libc::CLONE_NEWPID);
    };

    // Uncomment this block to pass the first stage!
    let args: Vec<_> = std::env::args().collect();
    let command = &args[3];
    let command_args = &args[4..];
    let output = std::process::Command::new(command)
        .args(command_args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| {
            format!(
                "Tried to run '{}' with arguments {:?}",
                command, command_args
            )
        })?;

    // Use child process exit code, fallback to 1
    let code = output.status.code().unwrap_or(1);

    std::process::exit(code);
}
