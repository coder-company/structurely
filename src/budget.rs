use anyhow::{ensure, Result};

pub struct ResourceBudget;

impl ResourceBudget {
    pub const MAX_QUERY_BYTES: usize = 16 * 1024;
    pub const MAX_IDENTIFIER_BYTES: usize = 4 * 1024;
    pub const MAX_RESULTS: usize = 100;
    pub const MAX_SEARCH_CANDIDATES: usize = Self::MAX_RESULTS * 4;
    pub const MAX_TRAVERSAL_DEPTH: usize = 20;
    pub const MAX_IMPACT_NODES: usize = 10_000;
    pub const MAX_IMPACT_EDGES: usize = 100_000;
    pub const MAX_SEARCH_TERMS: usize = 64;
    pub const MAX_BENCHMARK_ITERATIONS: usize = 1_000;
    pub const MAX_NODE_OFFSET: usize = 2_000;
    pub const MAX_NODE_LINES: usize = 2_000;
    pub const MAX_EXPLORE_RELATIONSHIPS: usize = 8;

    pub fn query(value: &str) -> Result<&str> {
        ensure!(!value.trim().is_empty(), "query must not be empty");
        ensure!(
            value.len() <= Self::MAX_QUERY_BYTES,
            "query exceeds the {}-byte limit",
            Self::MAX_QUERY_BYTES
        );
        Ok(value)
    }

    pub fn identifier(value: &str) -> Result<&str> {
        ensure!(!value.trim().is_empty(), "identifier must not be empty");
        ensure!(
            value.len() <= Self::MAX_IDENTIFIER_BYTES,
            "identifier exceeds the {}-byte limit",
            Self::MAX_IDENTIFIER_BYTES
        );
        Ok(value)
    }

    pub fn result_limit(value: usize) -> Result<usize> {
        ensure!(
            (1..=Self::MAX_RESULTS).contains(&value),
            "result limit must be between 1 and {}",
            Self::MAX_RESULTS
        );
        Ok(value)
    }

    pub(crate) fn search_candidate_limit(value: usize) -> Result<usize> {
        ensure!(
            (1..=Self::MAX_SEARCH_CANDIDATES).contains(&value),
            "search candidate limit must be between 1 and {}",
            Self::MAX_SEARCH_CANDIDATES
        );
        Ok(value)
    }

    pub fn traversal_depth(value: usize) -> Result<usize> {
        ensure!(
            (1..=Self::MAX_TRAVERSAL_DEPTH).contains(&value),
            "traversal depth must be between 1 and {}",
            Self::MAX_TRAVERSAL_DEPTH
        );
        Ok(value)
    }

    pub fn benchmark_iterations(value: usize) -> Result<usize> {
        ensure!(
            (1..=Self::MAX_BENCHMARK_ITERATIONS).contains(&value),
            "benchmark iterations must be between 1 and {}",
            Self::MAX_BENCHMARK_ITERATIONS
        );
        Ok(value)
    }

    pub fn node_offset(value: usize) -> Result<usize> {
        ensure!(
            value <= Self::MAX_NODE_OFFSET,
            "node offset must be between 0 and {}",
            Self::MAX_NODE_OFFSET
        );
        Ok(value)
    }

    pub fn node_lines(value: usize) -> Result<usize> {
        ensure!(
            (1..=Self::MAX_NODE_LINES).contains(&value),
            "node line limit must be between 1 and {}",
            Self::MAX_NODE_LINES
        );
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_resource_limits_are_inclusive_and_reject_overflow() {
        assert_eq!(
            ResourceBudget::result_limit(ResourceBudget::MAX_RESULTS).unwrap(),
            ResourceBudget::MAX_RESULTS
        );
        assert!(ResourceBudget::result_limit(0).is_err());
        assert!(ResourceBudget::result_limit(ResourceBudget::MAX_RESULTS + 1).is_err());
        assert!(ResourceBudget::query(&"q".repeat(ResourceBudget::MAX_QUERY_BYTES)).is_ok());
        assert!(ResourceBudget::query(&"q".repeat(ResourceBudget::MAX_QUERY_BYTES + 1)).is_err());
        assert!(ResourceBudget::traversal_depth(0).is_err());
        assert!(ResourceBudget::traversal_depth(ResourceBudget::MAX_TRAVERSAL_DEPTH + 1).is_err());
    }
}
