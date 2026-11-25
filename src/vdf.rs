//! Minimal parser for Valve's KeyValues text format (`libraryfolders.vdf`, `appmanifest_*.acf`).

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// First child with this key (case-insensitive, as Steam treats keys).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(items) => items
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Value::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    pub fn children(&self) -> &[(String, Value)] {
        match self {
            Value::Obj(items) => items,
            Value::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String),
    Open,
    Close,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '{' => out.push(Tok::Open),
            '}' => out.push(Tok::Close),
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                let mut s = String::new();
                loop {
                    match chars.next() {
                        None => return Err("unterminated string".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => return Err("unterminated escape".into()),
                        },
                        Some(other) => s.push(other),
                    }
                }
                out.push(Tok::Str(s));
            }
            _ => {
                // Unquoted token: runs until whitespace or a brace.
                let mut s = String::from(c);
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || n == '{' || n == '}' || n == '"' {
                        break;
                    }
                    s.push(n);
                    chars.next();
                }
                out.push(Tok::Str(s));
            }
        }
    }
    Ok(out)
}

/// Parses a document into a root object holding its top-level key/value pairs.
pub fn parse(src: &str) -> Result<Value, String> {
    let toks = tokenize(src)?;
    let mut pos = 0;
    let items = parse_items(&toks, &mut pos, false)?;
    Ok(Value::Obj(items))
}

fn parse_items(
    toks: &[Tok],
    pos: &mut usize,
    nested: bool,
) -> Result<Vec<(String, Value)>, String> {
    let mut items = Vec::new();
    loop {
        match toks.get(*pos) {
            None if nested => return Err("missing '}'".into()),
            None => return Ok(items),
            Some(Tok::Close) if nested => {
                *pos += 1;
                return Ok(items);
            }
            Some(Tok::Close) => return Err("unexpected '}'".into()),
            Some(Tok::Open) => return Err("unexpected '{'".into()),
            Some(Tok::Str(key)) => {
                *pos += 1;
                let value = match toks.get(*pos) {
                    Some(Tok::Str(v)) => {
                        *pos += 1;
                        Value::Str(v.clone())
                    }
                    Some(Tok::Open) => {
                        *pos += 1;
                        Value::Obj(parse_items(toks, pos, true)?)
                    }
                    _ => return Err(format!("key '{key}' has no value")),
                };
                items.push((key.clone(), value));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"apps"
		{
			"228980"		"1"
		}
	}
	// a comment
	"1"
	{
		"path"		"D:\\SteamLibrary"
		"apps" { "2929250" "123" }
	}
}
"#;

    #[test]
    fn parses_library_folders() {
        let doc = parse(LIBRARY).unwrap();
        let lf = doc.get("LibraryFolders").unwrap();
        let paths: Vec<_> = lf
            .children()
            .iter()
            .filter_map(|(_, v)| v.get("path")?.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![r"C:\Program Files (x86)\Steam", r"D:\SteamLibrary"]
        );
        let apps = lf.get("1").unwrap().get("apps").unwrap();
        assert_eq!(apps.get("2929250").unwrap().as_str(), Some("123"));
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse("\"a\" {").is_err());
        assert!(parse("\"a\"").is_err());
        assert!(parse("}").is_err());
        assert!(parse("\"unterminated").is_err());
        assert!(parse("{ }").is_err());
    }

    #[test]
    fn empty_is_empty_object() {
        assert_eq!(parse("  \n").unwrap(), Value::Obj(vec![]));
    }
}
