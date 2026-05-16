// Library entry point — re-exports the public NAT-PMP API so that integration
// tests (and potential downstream crates) can use `natpmp::{NatpmpClient, …}`
// without depending on the binary entry point.

pub mod gateway;
pub mod natpmp;
pub mod qbt;

pub use natpmp::{NatpmpClient, PortMapping, Protocol};

#[cfg(feature = "fuzz")]
pub use natpmp::validate_response_bytes;

#[cfg(all(feature = "fuzz", target_os = "linux"))]
pub use gateway::parse_proc_net_route;
