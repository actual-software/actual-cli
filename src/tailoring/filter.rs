use std::collections::HashSet;

use crate::api::types::Adr;

/// Remove ADRs whose IDs appear in the rejected list.
///
/// Returns a new `Vec<Adr>` containing only ADRs not in `rejected_ids`,
/// preserving the original order.
pub fn pre_filter_rejected(adrs: &[Adr], rejected_ids: &[String]) -> Vec<Adr> {
    if rejected_ids.is_empty() {
        return adrs.to_vec();
    }
    let rejected_set: HashSet<&str> = rejected_ids.iter().map(String::as_str).collect();
    adrs.iter()
        .filter(|adr| !rejected_set.contains(adr.id.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{AdrCategory, AppliesTo};

    fn make_adr(id: &str) -> Adr {
        Adr {
            id: id.to_string(),
            title: format!("ADR {id}"),
            context: None,
            policies: vec!["policy".to_string()],
            instructions: None,
            category: AdrCategory {
                id: "cat-1".to_string(),
                name: "General".to_string(),
                path: "General".to_string(),
            },
            applies_to: AppliesTo {
                languages: vec![],
                frameworks: vec![],
            },
            matched_projects: vec![],
        }
    }

    #[test]
    fn ten_adrs_three_rejected_returns_seven() {
        let adrs: Vec<Adr> = (0..10).map(|i| make_adr(&format!("adr-{i:03}"))).collect();
        let rejected = vec![
            "adr-002".to_string(),
            "adr-005".to_string(),
            "adr-008".to_string(),
        ];
        let result = pre_filter_rejected(&adrs, &rejected);
        assert_eq!(result.len(), 7);
        for adr in &result {
            assert!(!rejected.contains(&adr.id));
        }
    }

    #[test]
    fn empty_rejected_returns_all() {
        let adrs: Vec<Adr> = (0..10).map(|i| make_adr(&format!("adr-{i:03}"))).collect();
        let result = pre_filter_rejected(&adrs, &[]);
        assert_eq!(result.len(), 10);
    }

    #[test]
    fn all_rejected_returns_empty() {
        let adrs: Vec<Adr> = (0..3).map(|i| make_adr(&format!("adr-{i:03}"))).collect();
        let rejected: Vec<String> = (0..3).map(|i| format!("adr-{i:03}")).collect();
        let result = pre_filter_rejected(&adrs, &rejected);
        assert!(result.is_empty());
    }

    #[test]
    fn order_preserved() {
        let adrs: Vec<Adr> = (0..5).map(|i| make_adr(&format!("adr-{i:03}"))).collect();
        let rejected = vec!["adr-001".to_string(), "adr-003".to_string()];
        let result = pre_filter_rejected(&adrs, &rejected);
        let ids: Vec<&str> = result.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["adr-000", "adr-002", "adr-004"]);
    }
}
