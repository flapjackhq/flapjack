//! Helpers for resolving the process node identifier used in oplog version receipts.

fn normalized_node_id(configured_node_id: Option<String>) -> String {
    configured_node_id
        .filter(|node_id| !node_id.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(crate) fn configured_node_id() -> String {
    normalized_node_id(std::env::var("FLAPJACK_NODE_ID").ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_node_id_falls_back_for_missing_or_blank_values() {
        assert_eq!(normalized_node_id(None), "unknown");
        assert_eq!(normalized_node_id(Some(String::new())), "unknown");
        assert_eq!(normalized_node_id(Some("   ".to_string())), "unknown");
    }

    #[test]
    fn configured_node_id_preserves_non_blank_values() {
        assert_eq!(normalized_node_id(Some("node-a".to_string())), "node-a");
    }
}
