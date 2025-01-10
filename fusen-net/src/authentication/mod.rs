use std::sync::Arc;

use crate::frame::RegisterInfo;
use fusen_common::{BoxError, FusenFuture};

pub trait Authentication: 'static + Copy + Send + Sync {
    fn authentication(
        &self,
        register_info: Arc<RegisterInfo>,
    ) -> FusenFuture<Result<bool, BoxError>>;
}

#[derive(Clone, Copy)]
pub struct AuthenticationDefault;

impl Authentication for AuthenticationDefault {
    fn authentication(
        &self,
        _register_info: Arc<RegisterInfo>,
    ) -> FusenFuture<Result<bool, BoxError>> {
        Box::pin(async move { Ok(true) })
    }
}
