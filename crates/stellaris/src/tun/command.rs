// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{process::Stdio, time::Duration};

use tokio::io::AsyncReadExt as _;

use super::TunError;

pub(super) async fn run_route_command(
    program: &str,
    arguments: &[&str],
    command_timeout: Duration,
) -> Result<(), TunError> {
    let mut child = tokio::process::Command::new(program)
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(TunError::Native)?;
    let mut stderr_task = child.stderr.take().map(|stderr| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            stderr.take(8192).read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        })
    });
    let status = match tokio::time::timeout(command_timeout, child.wait()).await {
        Ok(result) => result.map_err(TunError::Native)?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            if let Some(task) = stderr_task.take() {
                let _ = task.await;
            }
            return Err(TunError::RouteCommandTimeout {
                program: program.to_owned(),
            });
        }
    };
    let stderr_bytes = match stderr_task {
        Some(task) => task
            .await
            .map_err(|error| TunError::Other(Box::new(error)))?
            .map_err(TunError::Native)?,
        None => Vec::new(),
    };
    if status.success() {
        return Ok(());
    }
    Err(TunError::RouteCommand {
        program: program.to_owned(),
        status: status.code(),
        stderr: String::from_utf8_lossy(&stderr_bytes).trim().to_owned(),
    })
}
