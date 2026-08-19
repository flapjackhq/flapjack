//! Stub summary for language.rs.
use std::fmt;
use std::str::FromStr;

/// A validated language code matching Algolia's supported set.
///
/// All ~70 languages Algolia recognizes, stored as lowercase two-letter codes
/// (plus `PtBr` for Brazilian Portuguese).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageCode {
    Af,
    Ar,
    Az,
    Bn,
    Bg,
    Ca,
    Cs,
    Cy,
    Da,
    De,
    El,
    En,
    Eo,
    Es,
    Et,
    Eu,
    Fa,
    Fi,
    Fo,
    Fr,
    Ga,
    Gl,
    He,
    Hi,
    Hu,
    Hy,
    Id,
    It,
    Ja,
    Ka,
    Kk,
    Ko,
    Ku,
    Ky,
    Lt,
    Lv,
    Mi,
    Mn,
    Mr,
    Ms,
    Mt,
    Nl,
    No,
    Ns,
    Pl,
    Ps,
    Pt,
    PtBr,
    Qu,
    Ro,
    Ru,
    Sk,
    Sq,
    Sv,
    Sw,
    Ta,
    Te,
    Tl,
    Tn,
    Tr,
    Tt,
    Th,
    Uk,
    Ur,
    Uz,
    Zh,
}

impl LanguageCode {
    /// The canonical string form (lowercase, e.g. "en", "pt-br").
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Af => "af",
            Self::Ar => "ar",
            Self::Az => "az",
            Self::Bn => "bn",
            Self::Bg => "bg",
            Self::Ca => "ca",
            Self::Cs => "cs",
            Self::Cy => "cy",
            Self::Da => "da",
            Self::De => "de",
            Self::El => "el",
            Self::En => "en",
            Self::Eo => "eo",
            Self::Es => "es",
            Self::Et => "et",
            Self::Eu => "eu",
            Self::Fa => "fa",
            Self::Fi => "fi",
            Self::Fo => "fo",
            Self::Fr => "fr",
            Self::Ga => "ga",
            Self::Gl => "gl",
            Self::He => "he",
            Self::Hi => "hi",
            Self::Hu => "hu",
            Self::Hy => "hy",
            Self::Id => "id",
            Self::It => "it",
            Self::Ja => "ja",
            Self::Ka => "ka",
            Self::Kk => "kk",
            Self::Ko => "ko",
            Self::Ku => "ku",
            Self::Ky => "ky",
            Self::Lt => "lt",
            Self::Lv => "lv",
            Self::Mi => "mi",
            Self::Mn => "mn",
            Self::Mr => "mr",
            Self::Ms => "ms",
            Self::Mt => "mt",
            Self::Nl => "nl",
            Self::No => "no",
            Self::Ns => "ns",
            Self::Pl => "pl",
            Self::Ps => "ps",
            Self::Pt => "pt",
            Self::PtBr => "pt-br",
            Self::Qu => "qu",
            Self::Ro => "ro",
            Self::Ru => "ru",
            Self::Sk => "sk",
            Self::Sq => "sq",
            Self::Sv => "sv",
            Self::Sw => "sw",
            Self::Ta => "ta",
            Self::Te => "te",
            Self::Tl => "tl",
            Self::Tn => "tn",
            Self::Tr => "tr",
            Self::Tt => "tt",
            Self::Th => "th",
            Self::Uk => "uk",
            Self::Ur => "ur",
            Self::Uz => "uz",
            Self::Zh => "zh",
        }
    }

    /// Whether this is a CJK language requiring special tokenization.
    pub fn is_cjk(&self) -> bool {
        matches!(self, Self::Ja | Self::Zh | Self::Ko)
    }

    /// Whether this language has decompound support.
    pub fn supports_decompound(&self) -> bool {
        matches!(self, Self::De | Self::Nl)
    }
}

impl fmt::Display for LanguageCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LanguageCode {
    type Err = UnknownLanguageCode;

    /// TODO: Document LanguageCode.from_str.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_lowercase();
        match lower.as_str() {
            "af" => Ok(Self::Af),
            "ar" => Ok(Self::Ar),
            "az" => Ok(Self::Az),
            "bn" => Ok(Self::Bn),
            "bg" => Ok(Self::Bg),
            "ca" => Ok(Self::Ca),
            "cs" => Ok(Self::Cs),
            "cy" => Ok(Self::Cy),
            "da" => Ok(Self::Da),
            "de" => Ok(Self::De),
            "el" => Ok(Self::El),
            "en" => Ok(Self::En),
            "eo" => Ok(Self::Eo),
            "es" => Ok(Self::Es),
            "et" => Ok(Self::Et),
            "eu" => Ok(Self::Eu),
            "fa" => Ok(Self::Fa),
            "fi" => Ok(Self::Fi),
            "fo" => Ok(Self::Fo),
            "fr" => Ok(Self::Fr),
            "ga" => Ok(Self::Ga),
            "gl" => Ok(Self::Gl),
            "he" => Ok(Self::He),
            "hi" => Ok(Self::Hi),
            "hu" => Ok(Self::Hu),
            "hy" => Ok(Self::Hy),
            "id" => Ok(Self::Id),
            "it" => Ok(Self::It),
            "ja" => Ok(Self::Ja),
            "ka" => Ok(Self::Ka),
            "kk" => Ok(Self::Kk),
            "ko" => Ok(Self::Ko),
            "ku" => Ok(Self::Ku),
            "ky" => Ok(Self::Ky),
            "lt" => Ok(Self::Lt),
            "lv" => Ok(Self::Lv),
            "mi" => Ok(Self::Mi),
            "mn" => Ok(Self::Mn),
            "mr" => Ok(Self::Mr),
            "ms" => Ok(Self::Ms),
            "mt" => Ok(Self::Mt),
            "nl" => Ok(Self::Nl),
            "no" => Ok(Self::No),
            "ns" => Ok(Self::Ns),
            "pl" => Ok(Self::Pl),
            "ps" => Ok(Self::Ps),
            "pt" => Ok(Self::Pt),
            "pt-br" | "ptbr" => Ok(Self::PtBr),
            "qu" => Ok(Self::Qu),
            "ro" => Ok(Self::Ro),
            "ru" => Ok(Self::Ru),
            "sk" => Ok(Self::Sk),
            "sq" => Ok(Self::Sq),
            "sv" => Ok(Self::Sv),
            "sw" => Ok(Self::Sw),
            "ta" => Ok(Self::Ta),
            "te" => Ok(Self::Te),
            "tl" => Ok(Self::Tl),
            "tn" => Ok(Self::Tn),
            "tr" => Ok(Self::Tr),
            "tt" => Ok(Self::Tt),
            "th" => Ok(Self::Th),
            "uk" => Ok(Self::Uk),
            "ur" => Ok(Self::Ur),
            "uz" => Ok(Self::Uz),
            "zh" => Ok(Self::Zh),
            _ => Err(UnknownLanguageCode(s.to_string())),
        }
    }
}

/// Attempt to parse a language code string. Returns `None` for unknown codes
/// (logs a warning but does not error).
pub fn parse_language_code(s: &str) -> Option<LanguageCode> {
    match s.parse::<LanguageCode>() {
        Ok(code) => Some(code),
        Err(_) => {
            tracing::warn!("Unknown language code '{}', ignoring", s);
            None
        }
    }
}

/// Error returned when a string does not match any known language code.
#[derive(Debug, Clone)]
pub struct UnknownLanguageCode(pub String);

impl fmt::Display for UnknownLanguageCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown language code: '{}'", self.0)
    }
}

impl std::error::Error for UnknownLanguageCode {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_codes_parse() {
        let codes = [
            "en", "fr", "de", "ja", "zh", "ko", "es", "it", "pt", "ru", "ar", "nl",
        ];
        for code in &codes {
            let parsed: LanguageCode = code.parse().unwrap();
            assert_eq!(parsed.as_str(), *code);
        }
    }

    #[test]
    fn test_case_normalization() {
        assert_eq!("EN".parse::<LanguageCode>().unwrap(), LanguageCode::En);
        assert_eq!("Fr".parse::<LanguageCode>().unwrap(), LanguageCode::Fr);
        assert_eq!("DE".parse::<LanguageCode>().unwrap(), LanguageCode::De);
        assert_eq!("JA".parse::<LanguageCode>().unwrap(), LanguageCode::Ja);
    }

    #[test]
    fn test_pt_br_variants() {
        assert_eq!("pt-br".parse::<LanguageCode>().unwrap(), LanguageCode::PtBr);
        assert_eq!("PT-BR".parse::<LanguageCode>().unwrap(), LanguageCode::PtBr);
        assert_eq!("ptbr".parse::<LanguageCode>().unwrap(), LanguageCode::PtBr);
        // plain "pt" is Portuguese (not Brazilian)
        assert_eq!("pt".parse::<LanguageCode>().unwrap(), LanguageCode::Pt);
    }

    #[test]
    fn test_unknown_codes_error() {
        assert!("xx".parse::<LanguageCode>().is_err());
        assert!("".parse::<LanguageCode>().is_err());
        assert!("english".parse::<LanguageCode>().is_err());
        assert!("en-US".parse::<LanguageCode>().is_err());
    }

    #[test]
    fn test_parse_language_code_graceful() {
        assert_eq!(parse_language_code("fr"), Some(LanguageCode::Fr));
        assert_eq!(parse_language_code("xx"), None);
        assert_eq!(parse_language_code(""), None);
    }

    #[test]
    fn test_display_roundtrip() {
        let code = LanguageCode::PtBr;
        assert_eq!(code.to_string(), "pt-br");
        assert_eq!(code.to_string().parse::<LanguageCode>().unwrap(), code);
    }

    #[test]
    fn test_is_cjk() {
        assert!(LanguageCode::Ja.is_cjk());
        assert!(LanguageCode::Zh.is_cjk());
        assert!(LanguageCode::Ko.is_cjk());
        assert!(!LanguageCode::En.is_cjk());
        assert!(!LanguageCode::Fr.is_cjk());
    }

    #[test]
    fn test_supports_decompound() {
        assert!(LanguageCode::De.supports_decompound());
        assert!(LanguageCode::Nl.supports_decompound());
        assert!(!LanguageCode::Fi.supports_decompound());
        assert!(!LanguageCode::Da.supports_decompound());
        assert!(!LanguageCode::Sv.supports_decompound());
        assert!(!LanguageCode::No.supports_decompound());
        assert!(!LanguageCode::En.supports_decompound());
        assert!(!LanguageCode::Fr.supports_decompound());
    }
}
