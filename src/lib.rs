pub mod agent;
pub mod model;
pub mod monitor;
pub mod platform;
pub mod storage;
pub mod systemd;
pub mod tray;

pub const BUS_NAME: &str = "io.github.stabilizer.Agent";
pub const OBJECT_PATH: &str = "/io/github/stabilizer/Agent";
pub const INTERFACE: &str = "io.github.stabilizer.Agent1";
