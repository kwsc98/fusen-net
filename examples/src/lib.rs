use tracing_subscriber::fmt::writer::MakeWriterExt;

pub fn init_log() {
    let stdout = std::io::stdout.with_max_level(tracing::Level::DEBUG);
    tracing_subscriber::fmt()
        .with_writer(stdout)
        .with_line_number(true)
        .with_thread_ids(true)
        .init();
}


pub const CERT_PEM: &str = "-----BEGIN CERTIFICATE-----
MIICRTCCAeugAwIBAgIUC989yXgvAxWhnaTdCsk8JgYpvzkwCgYIKoZIzj0EAwIw
gYExCzAJBgNVBAYTAkpQMQ4wDAYDVQQIDAVDaGliYTETMBEGA1UEBwwKQ2hpYmEg
Q2l0eTEYMBYGA1UECgwPVGVzc2llci1Bc2hwb29sMRAwDgYDVQQDDAdsb2NhbGNh
MSEwHwYJKoZIhvcNAQkBFhJjYUBkZXZlbG9wLmxvY2FsY2EwIBcNMjQwMzIzMDAz
NDMxWhgPMjIwMzA4MjkwMDM0MzFaMIGBMQswCQYDVQQGEwJKUDEOMAwGA1UECAwF
Q2hpYmExEzARBgNVBAcMCkNoaWJhIENpdHkxGDAWBgNVBAoMD1Rlc3NpZXItQXNo
cG9vbDEQMA4GA1UEAwwHbG9jYWxjYTEhMB8GCSqGSIb3DQEJARYSY2FAZGV2ZWxv
cC5sb2NhbGNhMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEbrmwtR2bEj/hit5i
7Vkh1wl3UqAykQFN801EYZC93qUp7XW9OB0U9kMk5K67Qb7239oL678jwtgJdBeo
DHa6C6M9MDswOQYDVR0RBDIwMIIJbG9jYWxob3N0ggtxbGF3cy5xbGF3c4cEfwAA
AYcQAAAAAAAAAAAAAAAAAAAAATAKBggqhkjOPQQDAgNIADBFAiAFj6aDZVkJm5v+
/f1MW9JCaWSdgzREF8wXRy4cWqZp3gIhAKprkqZOpfU4m1PLMuOqoRvnqz/r77uN
6nK1RbKK1pbF
-----END CERTIFICATE-----";

pub const KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgRlCQqSxQrvgT3BU7
xHp9ymk5r0RY2jccZOom+64gEv6hRANCAARuubC1HZsSP+GK3mLtWSHXCXdSoDKR
AU3zTURhkL3epSntdb04HRT2QyTkrrtBvvbf2gvrvyPC2Al0F6gMdroL
-----END PRIVATE KEY-----";