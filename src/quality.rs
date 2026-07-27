use crate::{model::RelationshipKind, Engine};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::Path,
};

#[derive(Debug, Deserialize)]
struct QualityManifest {
    edges: Vec<ExpectedEdge>,
}

#[derive(Debug, Deserialize)]
struct ExpectedEdge {
    language: String,
    caller: String,
    callee: String,
}

#[derive(Debug, Serialize)]
pub struct QualityReport {
    pub passed: bool,
    pub expected_edges: usize,
    pub predicted_edges: usize,
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    pub precision: f64,
    pub recall: f64,
    pub languages: BTreeMap<String, LanguageQuality>,
}

#[derive(Debug, Default, Serialize)]
pub struct LanguageQuality {
    pub expected_edges: usize,
    pub predicted_edges: usize,
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
    pub precision: f64,
    pub recall: f64,
}

impl Engine {
    pub fn evaluate_quality(&self, manifest_path: impl AsRef<Path>) -> Result<QualityReport> {
        let manifest: QualityManifest = serde_json::from_slice(
            &fs::read(manifest_path.as_ref())
                .with_context(|| format!("read {}", manifest_path.as_ref().display()))?,
        )
        .context("parse quality manifest")?;
        let expected = manifest
            .edges
            .into_iter()
            .map(|edge| (edge.language, edge.caller, edge.callee))
            .collect::<BTreeSet<_>>();
        let snapshot = self.snapshot()?;
        let symbols = snapshot
            .symbols
            .iter()
            .map(|symbol| (symbol.id.as_str(), symbol))
            .collect::<HashMap<_, _>>();
        let evaluated_languages = expected
            .iter()
            .map(|edge| edge.0.as_str())
            .collect::<BTreeSet<_>>();
        let predicted = snapshot
            .relationships
            .iter()
            .filter(|edge| edge.kind == RelationshipKind::Calls)
            .filter_map(|edge| {
                let caller = symbols.get(edge.source_id.as_str())?;
                let callee = symbols.get(edge.target_id.as_str())?;
                let language = caller.language.to_string();
                (evaluated_languages.contains(language.as_str())
                    && caller.language == callee.language)
                    .then(|| (language, caller.name.clone(), callee.name.clone()))
            })
            .collect::<BTreeSet<_>>();

        let mut languages = BTreeMap::new();
        for language in evaluated_languages {
            let expected_count = expected.iter().filter(|edge| edge.0 == language).count();
            let predicted_count = predicted.iter().filter(|edge| edge.0 == language).count();
            let true_positives = expected
                .intersection(&predicted)
                .filter(|edge| edge.0 == language)
                .count();
            languages.insert(
                language.to_owned(),
                metrics(expected_count, predicted_count, true_positives),
            );
        }
        let true_positives = expected.intersection(&predicted).count();
        let totals = metrics(expected.len(), predicted.len(), true_positives);
        Ok(QualityReport {
            passed: totals.false_positives == 0 && totals.false_negatives == 0,
            expected_edges: totals.expected_edges,
            predicted_edges: totals.predicted_edges,
            true_positives,
            false_positives: totals.false_positives,
            false_negatives: totals.false_negatives,
            precision: totals.precision,
            recall: totals.recall,
            languages,
        })
    }
}

fn metrics(expected: usize, predicted: usize, true_positives: usize) -> LanguageQuality {
    LanguageQuality {
        expected_edges: expected,
        predicted_edges: predicted,
        true_positives,
        false_positives: predicted.saturating_sub(true_positives),
        false_negatives: expected.saturating_sub(true_positives),
        precision: ratio(true_positives, predicted),
        recall: ratio(true_positives, expected),
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}
