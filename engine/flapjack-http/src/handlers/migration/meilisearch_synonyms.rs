use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MeilisearchSynonym {
    pub(super) input: String,
    pub(super) alternatives: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MeilisearchSynonymShapeError {
    json_path: String,
}

impl MeilisearchSynonymShapeError {
    pub(super) fn json_path(&self) -> &str {
        &self.json_path
    }
}

pub(super) fn parse_meilisearch_synonym_payload(
    item: &Value,
) -> Result<MeilisearchSynonym, MeilisearchSynonymShapeError> {
    let Some((input, alternatives)) = item.as_object().and_then(|object| {
        if object.len() == 1 {
            object.iter().next()
        } else {
            None
        }
    }) else {
        return Err(shape_error("$"));
    };
    // Meilisearch accepts an input mapped to an empty alternatives list, so the
    // list may be empty; only a non-array value or an entry that is not a
    // non-empty string is malformed.
    let alternatives = alternatives
        .as_array()
        .ok_or_else(|| shape_error(format!("$.{input}")))?;

    let mut parsed = Vec::with_capacity(alternatives.len());
    for (index, alternative) in alternatives.iter().enumerate() {
        let Some(alternative) = alternative.as_str().filter(|value| !value.is_empty()) else {
            return Err(shape_error(format!("$.{input}[{index}]")));
        };
        parsed.push(alternative.to_string());
    }

    Ok(MeilisearchSynonym {
        input: input.clone(),
        alternatives: parsed,
    })
}

fn shape_error(json_path: impl Into<String>) -> MeilisearchSynonymShapeError {
    MeilisearchSynonymShapeError {
        json_path: json_path.into(),
    }
}
