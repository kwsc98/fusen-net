use std::time::Duration;

use examples::init_log;
use fusen_common::fusen_procedural_macro::StrategyDebug;
use fusen_net::{
    client::{self},
    shutdown::ShutdownV2,
};
use structopt::StructOpt;
use tokio::sync::mpsc;
use tracing::{debug, error, info};


