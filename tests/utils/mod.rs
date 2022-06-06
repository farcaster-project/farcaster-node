//! Various utils to help build functional tests.

use crate::config::NodeConfig;

pub mod config;

#[test]
fn utils_cfg_node_with_proto_serde() {
    let s = "---\nhost: \"localhost\"\nport: 123\nprotocol: \"http\"\n";
    let node: NodeConfig = serde_yaml::from_str(&s).unwrap();
    assert_eq!(
        node,
        NodeConfig {
            host: "localhost".to_string(),
            port: 123,
            protocol: Some("http".to_string()),
        }
    );
}

#[test]
fn utils_cfg_node_with_proto_display() {
    let node = NodeConfig {
        host: "localhost".to_string(),
        port: 123,
        protocol: Some("http".to_string()),
    };
    let display = node.to_string();
    assert_eq!(display, "http://localhost:123");
}

#[test]
fn utils_cfg_node_without_proto_serde() {
    let s = "---\nhost: \"localhost\"\nport: 123\n";
    let node: NodeConfig = serde_yaml::from_str(&s).unwrap();
    assert_eq!(
        node,
        NodeConfig {
            host: "localhost".to_string(),
            port: 123,
            protocol: None,
        }
    );
}

#[test]
fn utils_cfg_node_without_proto_display() {
    let node = NodeConfig {
        host: "localhost".to_string(),
        port: 123,
        protocol: None,
    };
    let display = node.to_string();
    assert_eq!(display, "localhost:123");
}
