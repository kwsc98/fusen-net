use crate::{
    error::FusenNetError,
    quic::{Endpoint, Quiclib, gm_quic::GmQuicEndpoint, quin::QuinnEndpoint, s2n::S2nEndpoint},
};

pub struct Server;

pub struct ServerConfig {
    pub port: u16,
    pub quic_lib: Quiclib,
    pub cert_pem: String,
    pub priv_key_pem: String,
}

impl Server {
    pub async fn run(config: ServerConfig) -> Result<(), FusenNetError> {
        let port = config.port;
        let cert_pem = config.cert_pem;
        let priv_key_pem = config.priv_key_pem;
        match config.quic_lib {
            Quiclib::GmQuic => {
                Self::handler(GmQuicEndpoint::make_server_endpoint(
                    port,
                    cert_pem.as_str(),
                    priv_key_pem.as_str(),
                )?)
                .await
            }
            Quiclib::Quin => {
                Self::handler(QuinnEndpoint::make_server_endpoint(
                    port,
                    cert_pem.as_str(),
                    priv_key_pem.as_str(),
                )?)
                .await
            }
            Quiclib::S2n => {
                Self::handler(S2nEndpoint::make_server_endpoint(
                    port,
                    cert_pem.as_str(),
                    priv_key_pem.as_str(),
                )?)
                .await
            }
        }
    }

    async fn handler(endpoint: impl Endpoint) -> Result<(), FusenNetError> {
        todo!()
    }
}
