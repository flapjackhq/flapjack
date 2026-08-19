//! Stub summary for plural_expansion.rs.
use super::super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PluralLanguageSpec {
    language: String,
    include_builtin: bool,
    custom_sets: Vec<Vec<String>>,
}

#[cfg(feature = "decompound")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct DecompoundLanguageSpec {
    language: String,
    include_builtin: bool,
    custom_parts_by_word: HashMap<String, Vec<String>>,
}

/// Build the plural expansion map from settings and query languages.
pub(super) fn build_plural_map(
    tenant_id: &str,
    settings: &Option<Arc<IndexSettings>>,
    ignore_plurals_override: Option<&crate::query::plurals::IgnorePluralsValue>,
    effective_query_languages: &[String],
    query_text_stopped: &str,
    dictionary_manager: Option<&Arc<crate::dictionaries::manager::DictionaryManager>>,
) -> Option<HashMap<String, Vec<String>>> {
    let ignore_plurals = effective_ignore_plurals(settings, ignore_plurals_override)?;

    if *ignore_plurals == crate::query::plurals::IgnorePluralsValue::Disabled {
        return None;
    }

    let resolved =
        crate::query::plurals::resolve_plural_languages(ignore_plurals, effective_query_languages);
    let plural_languages = normalize_query_languages(&resolved);
    let plural_specs =
        build_plural_language_specs(tenant_id, &plural_languages, dictionary_manager);
    if plural_specs.is_empty() {
        return None;
    }

    build_plural_expansion_map(query_text_stopped, &plural_specs)
}

/// Apply decompound processing to the plural map.
#[cfg(feature = "decompound")]
pub(super) fn apply_decompound(
    tenant_id: &str,
    query_text_stopped: &str,
    plural_map: Option<HashMap<String, Vec<String>>>,
    decompound_langs: &[String],
    decompound_keep_diacritics: &str,
    custom_normalization: &[(char, String)],
    dictionary_manager: Option<&Arc<crate::dictionaries::manager::DictionaryManager>>,
) -> Option<HashMap<String, Vec<String>>> {
    let decompound_specs =
        build_decompound_language_specs(tenant_id, decompound_langs, dictionary_manager);
    let mut map = plural_map.unwrap_or_default();
    for word in query_text_stopped.split_whitespace() {
        let normalized_word =
            normalize_for_search(word, decompound_keep_diacritics, custom_normalization);
        let extra_parts = decompound_parts_for_word(
            word,
            &decompound_specs,
            decompound_keep_diacritics,
            custom_normalization,
        );
        append_unique_terms_for_key(&mut map, normalized_word, extra_parts);
    }

    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

fn effective_ignore_plurals<'a>(
    settings: &'a Option<Arc<IndexSettings>>,
    ignore_plurals_override: Option<&'a crate::query::plurals::IgnorePluralsValue>,
) -> Option<&'a crate::query::plurals::IgnorePluralsValue> {
    ignore_plurals_override.or(settings.as_ref().map(|s| &s.ignore_plurals))
}

fn build_plural_language_specs(
    tenant_id: &str,
    plural_languages: &[String],
    dictionary_manager: Option<&Arc<crate::dictionaries::manager::DictionaryManager>>,
) -> Vec<PluralLanguageSpec> {
    plural_languages
        .iter()
        .filter_map(|language| {
            build_plural_language_spec(tenant_id, language.as_str(), dictionary_manager)
        })
        .collect()
}

/// Build a `PluralLanguageSpec` for a language by checking built-in plural support
/// and loading custom plural sets from the dictionary manager. Returns `None` if
/// neither source provides plural data.
fn build_plural_language_spec(
    tenant_id: &str,
    language: &str,
    dictionary_manager: Option<&Arc<crate::dictionaries::manager::DictionaryManager>>,
) -> Option<PluralLanguageSpec> {
    let mut include_builtin = crate::query::plurals::has_plural_support(language);
    let mut custom_sets = Vec::new();

    if let Some(dictionary_manager) = dictionary_manager {
        let dict_tenant = tenant_id;
        include_builtin = include_builtin
            && read_builtin_plural_setting(dictionary_manager, dict_tenant, language);
        custom_sets = read_custom_plural_sets(dictionary_manager, dict_tenant, language);
    }

    if include_builtin || !custom_sets.is_empty() {
        Some(PluralLanguageSpec {
            language: language.to_string(),
            include_builtin,
            custom_sets,
        })
    } else {
        None
    }
}

/// Query the dictionary manager for whether built-in plural rules are enabled for
/// a language. Defaults to `true` on error.
fn read_builtin_plural_setting(
    dictionary_manager: &crate::dictionaries::manager::DictionaryManager,
    tenant_id: &str,
    language: &str,
) -> bool {
    match dictionary_manager.use_builtin_plurals(tenant_id, language) {
        Ok(use_builtin) => use_builtin,
        Err(error) => {
            tracing::warn!(
                language = %language,
                error = %error,
                "Failed to check plural settings; defaulting to built-in plural rules"
            );
            true
        }
    }
}

/// Load custom plural word sets from the dictionary manager for a language. Returns
/// an empty vec on error, falling back to built-in rules only.
fn read_custom_plural_sets(
    dictionary_manager: &crate::dictionaries::manager::DictionaryManager,
    tenant_id: &str,
    language: &str,
) -> Vec<Vec<String>> {
    match dictionary_manager.custom_plural_sets(tenant_id, language) {
        Ok(custom_sets) => custom_sets
            .into_iter()
            .map(|set| {
                set.into_iter()
                    .map(|word| word.to_lowercase())
                    .collect::<Vec<_>>()
            })
            .collect(),
        Err(error) => {
            tracing::warn!(
                language = %language,
                error = %error,
                "Failed to load custom plural sets; continuing with built-in plural rules"
            );
            Vec::new()
        }
    }
}

/// Build a map from each query word to its plural forms across all language specs.
/// Only includes words that expand to more than one form.
fn build_plural_expansion_map(
    query_text_stopped: &str,
    plural_specs: &[PluralLanguageSpec],
) -> Option<HashMap<String, Vec<String>>> {
    let mut plural_expansion_map = HashMap::new();
    for query_word in query_text_stopped.split_whitespace() {
        let lower_word = query_word.to_lowercase();
        let forms = plural_forms_for_word(&lower_word, plural_specs);
        if forms.len() > 1 {
            plural_expansion_map.insert(lower_word, forms);
        }
    }
    if plural_expansion_map.is_empty() {
        None
    } else {
        Some(plural_expansion_map)
    }
}

/// Collect all plural forms for a word: built-in language-specific expansions plus
/// any custom sets containing the word. Deduplicates across sources.
fn plural_forms_for_word(word: &str, plural_specs: &[PluralLanguageSpec]) -> Vec<String> {
    let mut forms = vec![word.to_string()];
    for spec in plural_specs {
        if spec.include_builtin {
            push_unique_terms(
                &mut forms,
                crate::query::plurals::expand_plurals_for_lang(word, &spec.language),
            );
        }
        for custom_set in &spec.custom_sets {
            if custom_set.iter().any(|value| value == word) {
                push_unique_terms(&mut forms, custom_set.iter().cloned());
            }
        }
    }
    forms
}

#[cfg(feature = "decompound")]
fn build_decompound_language_specs(
    tenant_id: &str,
    decompound_languages: &[String],
    dictionary_manager: Option<&Arc<crate::dictionaries::manager::DictionaryManager>>,
) -> Vec<DecompoundLanguageSpec> {
    decompound_languages
        .iter()
        .filter_map(|language| {
            build_decompound_language_spec(tenant_id, language.as_str(), dictionary_manager)
        })
        .collect()
}

/// Build a `DecompoundLanguageSpec` for a language by checking the built-in
/// decompound setting and loading custom compound word mappings from the
/// dictionary manager.
#[cfg(feature = "decompound")]
fn build_decompound_language_spec(
    tenant_id: &str,
    language: &str,
    dictionary_manager: Option<&Arc<crate::dictionaries::manager::DictionaryManager>>,
) -> Option<DecompoundLanguageSpec> {
    let mut include_builtin = true;
    let mut custom_parts_by_word = HashMap::new();

    if let Some(dictionary_manager) = dictionary_manager {
        let dict_tenant = tenant_id;
        include_builtin =
            read_builtin_decompound_setting(dictionary_manager, dict_tenant, language);
        custom_parts_by_word =
            read_custom_decompound_map(dictionary_manager, dict_tenant, language);
    }

    if include_builtin || !custom_parts_by_word.is_empty() {
        Some(DecompoundLanguageSpec {
            language: language.to_string(),
            include_builtin,
            custom_parts_by_word,
        })
    } else {
        None
    }
}

/// Query the dictionary manager for whether the built-in compound dictionary is
/// enabled for a language. Checks the `Compounds` dictionary-name flag; defaults
/// to `true` on error.
#[cfg(feature = "decompound")]
fn read_builtin_decompound_setting(
    dictionary_manager: &crate::dictionaries::manager::DictionaryManager,
    tenant_id: &str,
    language: &str,
) -> bool {
    match dictionary_manager.is_standard_disabled(
        tenant_id,
        crate::dictionaries::DictionaryName::Compounds,
        language,
    ) {
        Ok(is_disabled) => !is_disabled,
        Err(error) => {
            tracing::warn!(
                language = %language,
                error = %error,
                "Failed to read compound settings; defaulting to built-in decompound behavior"
            );
            true
        }
    }
}

/// Load custom compound-word decomposition mappings from the dictionary manager.
/// Returns an empty map on error, falling back to built-in behavior only.
#[cfg(feature = "decompound")]
fn read_custom_decompound_map(
    dictionary_manager: &crate::dictionaries::manager::DictionaryManager,
    tenant_id: &str,
    language: &str,
) -> HashMap<String, Vec<String>> {
    match dictionary_manager.effective_compounds(tenant_id, language) {
        Ok(entries) => entries
            .into_iter()
            .map(|(word, parts)| {
                (
                    word.to_lowercase(),
                    parts.into_iter().map(|part| part.to_lowercase()).collect(),
                )
            })
            .collect(),
        Err(error) => {
            tracing::warn!(
                language = %language,
                error = %error,
                "Failed to load custom compounds; continuing with built-in decompound behavior"
            );
            HashMap::new()
        }
    }
}

/// Decompose a word into its constituent parts using both custom mappings and
/// (if enabled) the built-in decompound dictionary. Normalizes the word before lookup.
#[cfg(feature = "decompound")]
fn decompound_parts_for_word(
    word: &str,
    decompound_specs: &[DecompoundLanguageSpec],
    decompound_keep_diacritics: &str,
    custom_normalization: &[(char, String)],
) -> Vec<String> {
    let lower_word = word.to_lowercase();
    let normalized_word = normalize_for_search(
        &lower_word,
        decompound_keep_diacritics,
        custom_normalization,
    );
    let mut parts = Vec::new();

    for spec in decompound_specs {
        if spec.include_builtin {
            if let Some(builtin_parts) =
                crate::query::decompound::decompound_for_lang(&lower_word, &spec.language)
            {
                let normalized_parts = builtin_parts.into_iter().map(|part| {
                    normalize_for_search(&part, decompound_keep_diacritics, custom_normalization)
                });
                push_unique_terms(&mut parts, normalized_parts);
            }
        }

        if let Some(custom_parts) = spec
            .custom_parts_by_word
            .get(&lower_word)
            .or_else(|| spec.custom_parts_by_word.get(&normalized_word))
        {
            let normalized_parts = custom_parts.iter().map(|part| {
                normalize_for_search(part, decompound_keep_diacritics, custom_normalization)
            });
            push_unique_terms(&mut parts, normalized_parts);
        }
    }

    parts
}

#[cfg(feature = "decompound")]
fn append_unique_terms_for_key(
    map: &mut HashMap<String, Vec<String>>,
    key: String,
    terms: Vec<String>,
) {
    if terms.is_empty() {
        return;
    }
    let entry = map.entry(key).or_default();
    for term in terms {
        if !entry.contains(&term) {
            entry.push(term);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// TODO: Document plural_forms_for_word_merges_builtin_and_custom_without_duplicates.
    #[test]
    fn plural_forms_for_word_merges_builtin_and_custom_without_duplicates() {
        let plural_specs = vec![
            PluralLanguageSpec {
                language: "en".to_string(),
                include_builtin: false,
                custom_sets: vec![vec![
                    "shoe".to_string(),
                    "shoes".to_string(),
                    "shoe".to_string(),
                ]],
            },
            PluralLanguageSpec {
                language: "fr".to_string(),
                include_builtin: false,
                custom_sets: vec![vec!["shoe".to_string(), "chaussure".to_string()]],
            },
        ];

        let forms = plural_forms_for_word("shoe", &plural_specs);

        assert_eq!(forms, vec!["shoe", "shoes", "chaussure"]);
    }

    /// TODO: Document decompound_parts_for_word_merges_custom_and_normalized_keys.
    #[cfg(feature = "decompound")]
    #[test]
    fn decompound_parts_for_word_merges_custom_and_normalized_keys() {
        let decompound_specs = vec![DecompoundLanguageSpec {
            language: "de".to_string(),
            include_builtin: false,
            custom_parts_by_word: HashMap::from([(
                "wasserkraft".to_string(),
                vec![
                    "wasser".to_string(),
                    "kraft".to_string(),
                    "wasser".to_string(),
                ],
            )]),
        }];

        let parts = decompound_parts_for_word("Wasserkraft", &decompound_specs, "", &[]);

        assert_eq!(parts, vec!["wasser", "kraft"]);
    }
}
