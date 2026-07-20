// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{
    fmt::Write as _,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::{ConfigError, validate_node_id};

#[derive(Debug, Error)]
pub enum TokenError {
    #[error(transparent)]
    InvalidNode(#[from] ConfigError),
    #[error("cannot create token file {path}: {source}")]
    Create {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write token file {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[cfg(windows)]
    #[error("cannot restrict the ACL for token file {path}: {source}")]
    RestrictAcl {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub struct GeneratedToken {
    pub node_id: String,
    pub token_sha256: String,
}

pub fn generate(node_id: &str, output: &Path) -> Result<GeneratedToken, TokenError> {
    validate_node_id(node_id)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(output).map_err(|source| TokenError::Create {
        path: output.to_path_buf(),
        source,
    })?;
    #[cfg(windows)]
    if let Err(source) = crate::windows_security::restrict_secret_acl(output) {
        drop(file);
        let _ = std::fs::remove_file(output);
        return Err(TokenError::RestrictAcl {
            path: output.to_path_buf(),
            source,
        });
    }

    let mut random = [0_u8; 32];
    OsRng.fill_bytes(&mut random);
    let token = format!("fsn1_{}", URL_SAFE_NO_PAD.encode(random));
    if let Err(source) = writeln!(file, "{token}").and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(output);
        return Err(TokenError::Write {
            path: output.to_path_buf(),
            source,
        });
    }

    let digest = Sha256::digest(token.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(GeneratedToken {
        node_id: node_id.to_owned(),
        token_sha256: format!("sha256:{hex}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn generated_token_is_private_valid_and_never_overwritten() {
        let mut suffix = [0_u8; 8];
        OsRng.fill_bytes(&mut suffix);
        let path = std::env::temp_dir().join(format!(
            "fusen-net-token-test-{}-{}",
            std::process::id(),
            u64::from_ne_bytes(suffix)
        ));

        let generated = generate("edge-test", &path).expect("generate token");
        let contents = fs::read_to_string(&path).expect("read token");
        let token = contents.trim_end();
        fusen_net::address::NodeToken::parse(token).expect("valid token format");
        assert_eq!(generated.node_id, "edge-test");
        assert_eq!(
            generated.token_sha256,
            format!("sha256:{:x}", Sha256::digest(token.as_bytes()))
        );
        assert!(generate("edge-test", &path).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path)
                    .expect("token metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        assert!(
            crate::windows_security::secret_acl_is_restricted(&path)
                .expect("validate generated token ACL")
        );

        fs::remove_file(path).expect("remove test token");
    }
}
