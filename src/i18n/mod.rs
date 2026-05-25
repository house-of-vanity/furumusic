mod phrases;

pub use phrases::Translations;

use cot::request::RequestHead;
use cot::request::extractors::FromRequestHead;
use serde::{Deserialize, Serialize};

impl Translations {
    pub fn app_version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

// ---------------------------------------------------------------------------
// Lang enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }

    pub fn from_code(s: &str) -> Option<Self> {
        match s {
            "en" => Some(Lang::En),
            "ru" => Some(Lang::Ru),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// translations! macro
// ---------------------------------------------------------------------------

macro_rules! translations {
    ( $( $key:ident : $en:expr , $ru:expr );* $(;)? ) => {
        #[derive(Debug)]
        #[allow(dead_code)]
        pub struct Translations {
            pub lang: $crate::i18n::Lang,
            $( pub $key: &'static str, )*
        }

        static EN: Translations = Translations {
            lang: $crate::i18n::Lang::En,
            $( $key: $en, )*
        };

        static RU: Translations = Translations {
            lang: $crate::i18n::Lang::Ru,
            $( $key: $ru, )*
        };

        impl Translations {
            pub fn for_lang(lang: $crate::i18n::Lang) -> &'static Self {
                match lang {
                    $crate::i18n::Lang::En => &EN,
                    $crate::i18n::Lang::Ru => &RU,
                }
            }
        }
    };
}

pub(crate) use translations;

// ---------------------------------------------------------------------------
// Cookie helpers
// ---------------------------------------------------------------------------

const COOKIE_NAME: &str = "furu_lang";

/// Build a `Set-Cookie` header value that persists the language choice for 1 year.
pub fn lang_cookie(lang: Lang) -> String {
    format!(
        "{COOKIE_NAME}={}; Path=/; SameSite=Lax; Max-Age=31536000",
        lang.code()
    )
}

/// Parse `furu_lang` from the `Cookie` request header.
fn lang_from_cookie(headers: &cot::http::HeaderMap) -> Option<Lang> {
    let raw = headers.get(cot::http::header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("furu_lang=") {
            return Lang::from_code(value.trim());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Accept-Language parsing
// ---------------------------------------------------------------------------

/// Parse the Accept-Language header and return the best matching `Lang`.
fn parse_accept_language(header: &str) -> Option<Lang> {
    let mut langs: Vec<(&str, u16)> = header
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            let (tag, quality) = if let Some((tag, q)) = part.split_once(";q=") {
                let q = q.trim().parse::<f32>().ok()?;
                (tag.trim(), (q * 1000.0) as u16)
            } else {
                (part, 1000)
            };
            Some((tag, quality))
        })
        .collect();

    langs.sort_by(|a, b| b.1.cmp(&a.1));

    for (tag, _) in langs {
        let primary = tag.split('-').next().unwrap_or(tag);
        if let Some(lang) = Lang::from_code(primary) {
            return Some(lang);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Language resolution
// ---------------------------------------------------------------------------

fn resolve_lang(headers: &cot::http::HeaderMap) -> Lang {
    // 1. Explicit cookie override.
    if let Some(lang) = lang_from_cookie(headers) {
        return lang;
    }

    // 2. Accept-Language header.
    if let Some(value) = headers.get(cot::http::header::ACCEPT_LANGUAGE) {
        if let Ok(s) = value.to_str() {
            if let Some(lang) = parse_accept_language(s) {
                return lang;
            }
        }
    }

    // 3. Default.
    Lang::En
}

// ---------------------------------------------------------------------------
// I18n extractor
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct I18n {
    pub lang: Lang,
    pub t: &'static Translations,
}

impl FromRequestHead for I18n {
    async fn from_request_head(head: &RequestHead) -> cot::Result<Self> {
        let lang = resolve_lang(&head.headers);
        Ok(I18n {
            lang,
            t: Translations::for_lang(lang),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_roundtrip() {
        assert_eq!(Lang::from_code("en"), Some(Lang::En));
        assert_eq!(Lang::from_code("ru"), Some(Lang::Ru));
        assert_eq!(Lang::from_code("de"), None);
        assert_eq!(Lang::En.code(), "en");
        assert_eq!(Lang::Ru.code(), "ru");
    }

    #[test]
    fn parse_simple_accept_language() {
        assert_eq!(parse_accept_language("ru"), Some(Lang::Ru));
        assert_eq!(parse_accept_language("en-US"), Some(Lang::En));
    }

    #[test]
    fn parse_weighted_accept_language() {
        assert_eq!(
            parse_accept_language("en-US,en;q=0.9,ru;q=0.8"),
            Some(Lang::En)
        );
        assert_eq!(
            parse_accept_language("ru-RU,ru;q=0.9,en;q=0.5"),
            Some(Lang::Ru)
        );
    }

    #[test]
    fn parse_unknown_falls_through() {
        assert_eq!(parse_accept_language("de;q=1.0,ru;q=0.5"), Some(Lang::Ru));
        assert_eq!(parse_accept_language("de,fr,ja"), None);
    }

    #[test]
    fn cookie_parsing() {
        let mut headers = cot::http::HeaderMap::new();
        headers.insert(
            cot::http::header::COOKIE,
            "other=x; furu_lang=ru; foo=bar".parse().unwrap(),
        );
        assert_eq!(lang_from_cookie(&headers), Some(Lang::Ru));
    }

    #[test]
    fn cookie_missing() {
        let headers = cot::http::HeaderMap::new();
        assert_eq!(lang_from_cookie(&headers), None);
    }
}
