use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportDescription {
    pub name: String,
    pub mode: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeReport {
    pub ok: bool,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendReport {
    pub accepted: bool,
    pub exit_status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub captured_path: Option<String>,
}
