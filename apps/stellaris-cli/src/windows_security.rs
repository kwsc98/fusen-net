// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Windows secret-file ACL policy shared by generation and validation.

use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(5);

const CHECK_ACL: &str = r#"
$ErrorActionPreference = 'Stop'
$acl = Microsoft.PowerShell.Security\Get-Acl -LiteralPath $env:STELLARIS_SECRET_PATH
$current = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$allowed = @($current, 'S-1-5-18', 'S-1-5-32-544')
$descriptor = [Security.AccessControl.RawSecurityDescriptor]::new(
    $acl.GetSecurityDescriptorBinaryForm(),
    0
)
if ($null -eq $descriptor.DiscretionaryAcl) { exit 43 }
$owner = $acl.GetOwner(
    [Security.Principal.SecurityIdentifier]
).Value
if ($allowed -notcontains $owner) { exit 41 }
$rules = $acl.GetAccessRules(
    $true,
    $true,
    [Security.Principal.SecurityIdentifier]
)
foreach ($rule in $rules) {
    if ($rule.AccessControlType -ne 'Allow') { continue }
    if ($allowed -notcontains $rule.IdentityReference.Value) { exit 42 }
}
exit 0
"#;

const RESTRICT_ACL: &str = r#"
$ErrorActionPreference = 'Stop'
$current = [Security.Principal.WindowsIdentity]::GetCurrent().User
$system = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
$administrators = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
$acl = [Security.AccessControl.FileSecurity]::new()
$acl.SetOwner($current)
$acl.SetAccessRuleProtection($true, $false)
foreach ($principal in @($current, $system, $administrators)) {
    $rule = [Security.AccessControl.FileSystemAccessRule]::new(
        $principal,
        [Security.AccessControl.FileSystemRights]::FullControl,
        [Security.AccessControl.AccessControlType]::Allow
    )
    [void]$acl.AddAccessRule($rule)
}
Microsoft.PowerShell.Security\Set-Acl `
    -LiteralPath $env:STELLARIS_SECRET_PATH `
    -AclObject $acl
exit 0
"#;

pub fn secret_acl_is_restricted(path: &Path) -> io::Result<bool> {
    match run_powershell(CHECK_ACL, path)?.code() {
        Some(0) => Ok(true),
        Some(41 | 42 | 43) => Ok(false),
        status => Err(io::Error::other(format!(
            "PowerShell ACL validation failed with status {status:?}"
        ))),
    }
}

pub fn restrict_secret_acl(path: &Path) -> io::Result<()> {
    let status = run_powershell(RESTRICT_ACL, path)?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "PowerShell ACL update failed with status {:?}",
            status.code()
        )))
    }
}

fn run_powershell(script: &str, path: &Path) -> io::Result<std::process::ExitStatus> {
    let mut child = Command::new(system_powershell()?)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("STELLARIS_SECRET_PATH", path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= POWERSHELL_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "PowerShell ACL operation exceeded five seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn system_powershell() -> io::Result<PathBuf> {
    let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "SystemRoot is unavailable; cannot locate Windows PowerShell",
        )
    })?;
    Ok(PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngCore, rngs::OsRng};
    use std::fs;

    const GRANT_EVERYONE: &str = r#"
$ErrorActionPreference = 'Stop'
$acl = Microsoft.PowerShell.Security\Get-Acl -LiteralPath $env:STELLARIS_SECRET_PATH
$everyone = [Security.Principal.SecurityIdentifier]::new('S-1-1-0')
$rule = [Security.AccessControl.FileSystemAccessRule]::new(
    $everyone,
    [Security.AccessControl.FileSystemRights]::Read,
    [Security.AccessControl.AccessControlType]::Allow
)
[void]$acl.AddAccessRule($rule)
Microsoft.PowerShell.Security\Set-Acl `
    -LiteralPath $env:STELLARIS_SECRET_PATH `
    -AclObject $acl
exit 0
"#;

    #[test]
    fn restricted_acl_rejects_an_added_everyone_rule() {
        let mut suffix = [0_u8; 8];
        OsRng.fill_bytes(&mut suffix);
        let path = std::env::temp_dir().join(format!(
            "stellaris-acl-test-{}-{}",
            std::process::id(),
            u64::from_ne_bytes(suffix)
        ));
        fs::write(&path, b"fixture").expect("create ACL fixture");

        restrict_secret_acl(&path).expect("restrict fixture ACL");
        assert!(secret_acl_is_restricted(&path).expect("validate restricted ACL"));
        assert!(
            run_powershell(GRANT_EVERYONE, &path)
                .expect("grant Everyone")
                .success()
        );
        assert!(!secret_acl_is_restricted(&path).expect("reject broad ACL"));

        fs::remove_file(path).expect("remove ACL fixture");
    }
}
