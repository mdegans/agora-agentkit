//! Batch grouping algorithm.
//!
//! Groups [`WorkItem`]s into [`BatchGroup`]s optimized for cache reuse.
//! Sort priority: model > prefix hash > context length bucket.

use std::collections::BTreeMap;

use super::WorkItem;

/// Configuration for the grouping algorithm.
#[derive(Debug, Clone)]
pub struct GroupingConfig {
    /// Context length bucket size in tokens. Items whose token counts
    /// fall in the same bucket (token_count / bucket_size) are grouped
    /// together to minimize Ollama memory reallocation.
    pub context_length_bucket: u32,
}

impl Default for GroupingConfig {
    fn default() -> Self {
        Self {
            context_length_bucket: 4096,
        }
    }
}

/// A group of work items that should be submitted together.
///
/// All items in a group share the same model and prefix hash,
/// maximizing cache reuse.
#[derive(Debug)]
pub struct BatchGroup<P> {
    /// Model identifier shared by all items in this group.
    pub model: String,
    /// Prefix hash shared by all items in this group.
    pub prefix_hash: u64,
    /// Context length bucket (token_count / bucket_size).
    pub context_bucket: u32,
    /// The work items in this group.
    pub items: Vec<WorkItem<P>>,
}

/// Composite key for grouping: (model, prefix_hash, context_bucket).
///
/// Uses `BTreeMap` ordering: model (String) > prefix_hash (u64) >
/// context_bucket (u32), which matches our desired sort priority.
type GroupKey = (String, u64, u32);

/// Group work items into [`BatchGroup`]s by (model, prefix_hash, context_bucket).
///
/// Items are grouped by the composite key. Within each group, items are
/// in their original order. The groups themselves are sorted by key
/// (model first, then prefix_hash, then context_bucket).
pub fn group_work_items<P>(
    items: Vec<WorkItem<P>>,
    config: &GroupingConfig,
) -> Vec<BatchGroup<P>> {
    let bucket_size = config.context_length_bucket.max(1); // avoid div by zero

    let mut groups: BTreeMap<GroupKey, Vec<WorkItem<P>>> = BTreeMap::new();

    for item in items {
        let bucket = item.token_count / bucket_size;
        let key = (item.model.clone(), item.prefix_hash, bucket);
        groups.entry(key).or_default().push(item);
    }

    groups
        .into_iter()
        .map(|((model, prefix_hash, context_bucket), items)| BatchGroup {
            model,
            prefix_hash,
            context_bucket,
            items,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::ids::AgentId;

    use super::*;
    use super::super::CycleStep;

    fn item(
        model: &str,
        prefix_hash: u64,
        token_count: u32,
    ) -> WorkItem<()> {
        WorkItem {
            agent_id: AgentId::new(),
            prompt: (),
            step: CycleStep::Think,
            prefix_hash,
            model: model.to_string(),
            queued_at: Instant::now(),
            token_count,
        }
    }

    #[test]
    fn groups_by_model() {
        let items = vec![
            item("claude", 100, 5000),
            item("cogito", 100, 5000),
            item("claude", 100, 5000),
        ];

        let groups = group_work_items(items, &GroupingConfig::default());
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].model, "claude");
        assert_eq!(groups[0].items.len(), 2);
        assert_eq!(groups[1].model, "cogito");
        assert_eq!(groups[1].items.len(), 1);
    }

    #[test]
    fn groups_by_prefix_hash() {
        let items = vec![
            item("claude", 100, 5000),
            item("claude", 200, 5000),
            item("claude", 100, 5000),
        ];

        let groups = group_work_items(items, &GroupingConfig::default());
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].prefix_hash, 100);
        assert_eq!(groups[0].items.len(), 2);
        assert_eq!(groups[1].prefix_hash, 200);
    }

    #[test]
    fn groups_by_context_bucket() {
        let config = GroupingConfig {
            context_length_bucket: 4096,
        };

        let items = vec![
            item("claude", 100, 4000),  // bucket 0
            item("claude", 100, 5000),  // bucket 1
            item("claude", 100, 4500),  // bucket 1
            item("claude", 100, 12000), // bucket 2
        ];

        let groups = group_work_items(items, &config);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].context_bucket, 0);
        assert_eq!(groups[0].items.len(), 1);
        assert_eq!(groups[1].context_bucket, 1);
        assert_eq!(groups[1].items.len(), 2);
        assert_eq!(groups[2].context_bucket, 2);
        assert_eq!(groups[2].items.len(), 1);
    }

    #[test]
    fn empty_input() {
        let groups = group_work_items::<()>(vec![], &GroupingConfig::default());
        assert!(groups.is_empty());
    }
}
