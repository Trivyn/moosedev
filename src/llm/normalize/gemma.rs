//! Gemma's own tool-call syntax, written as content when the server does not
//! parse it into a native call: `<|tool_call>call:reply{message:<|"|>…<|"|>,
//! then:<|"|>wait<|"|>}<tool_call|>`, keys bare and strings delimited by
//! `<|"|>` with raw quotes and newlines inside. LM Studio with gemma-4-26b-a4b
//! returned it with the `<|tool_call>call:` prefix already stripped (badciv
//! e3c533b4, harness actions 19 and 32). The generic JSON scan cannot read it:
//! a `"` inside a `<|"|>` string toggles its string state.
use super::Dialect;
use crate::llm::ToolCall;
use serde_json::{Map, Number, Value};

pub struct Gemma;

const OPEN: &str = "<|tool_call>";
const CLOSE: &str = "<tool_call|>";
const QUOTE: &str = "<|\"|>";
/// Nesting beyond this is not a tool call; the bound keeps recursion shallow.
const MAX_DEPTH: usize = 32;

impl Dialect for Gemma {
    fn name(&self) -> &'static str {
        "gemma"
    }

    /// The whole content must be one call: anything else around it is `None`.
    fn text_call(&self, content: &str) -> Option<ToolCall> {
        let mut parser = Parser(content.trim());
        parser.eat(OPEN);
        parser.skip_whitespace();
        parser.eat("call:");
        let name = parser.identifier()?.to_owned();
        parser.skip_whitespace();
        let arguments = parser.object(0)?;
        parser.skip_whitespace();
        parser.eat(CLOSE);
        parser.skip_whitespace();
        parser.0.is_empty().then(|| ToolCall {
            id: None,
            name,
            arguments: Value::Object(arguments).to_string(),
        })
    }
}

/// The unread rest of the input.
struct Parser<'a>(&'a str);

impl<'a> Parser<'a> {
    fn eat(&mut self, token: &str) -> bool {
        match self.0.strip_prefix(token) {
            Some(rest) => {
                self.0 = rest;
                true
            }
            None => false,
        }
    }

    fn skip_whitespace(&mut self) {
        self.0 = self.0.trim_start();
    }

    /// A tool name or bare key.
    fn identifier(&mut self) -> Option<&'a str> {
        let end = self
            .0
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(self.0.len());
        let (identifier, rest) = self.0.split_at(end);
        self.0 = rest;
        (!identifier.is_empty()).then_some(identifier)
    }

    fn object(&mut self, depth: usize) -> Option<Map<String, Value>> {
        if depth > MAX_DEPTH || !self.eat("{") {
            return None;
        }
        let mut object = Map::new();
        self.skip_whitespace();
        if self.eat("}") {
            return Some(object);
        }
        loop {
            self.skip_whitespace();
            let key = self.identifier()?.to_owned();
            self.skip_whitespace();
            if !self.eat(":") {
                return None;
            }
            let value = self.value(depth)?;
            object.insert(key, value);
            self.skip_whitespace();
            if self.eat("}") {
                return Some(object);
            }
            if !self.eat(",") {
                return None;
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<Vec<Value>> {
        if depth > MAX_DEPTH || !self.eat("[") {
            return None;
        }
        let mut array = Vec::new();
        self.skip_whitespace();
        if self.eat("]") {
            return Some(array);
        }
        loop {
            array.push(self.value(depth)?);
            self.skip_whitespace();
            if self.eat("]") {
                return Some(array);
            }
            if !self.eat(",") {
                return None;
            }
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        self.skip_whitespace();
        if self.eat(QUOTE) {
            let (text, rest) = self.0.split_once(QUOTE)?;
            self.0 = rest;
            return Some(Value::String(text.to_owned()));
        }
        match self.0.chars().next()? {
            '{' => self.object(depth + 1).map(Value::Object),
            '[' => self.array(depth + 1).map(Value::Array),
            _ => {
                // A literal or number runs to the next delimiter.
                let end = self.0.find([',', '}', ']']).unwrap_or(self.0.len());
                let (token, rest) = self.0.split_at(end);
                self.0 = rest;
                match token.trim_end() {
                    "true" => Some(Value::Bool(true)),
                    "false" => Some(Value::Bool(false)),
                    "null" => Some(Value::Null),
                    number => serde_json::from_str::<Number>(number)
                        .ok()
                        .map(Value::Number),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// badciv e3c533b4, harness action 19: a replace whose Rust source holds
    /// real quotes and newlines.
    const REPLACE: &str = r#"replace{file:<|"|>badciv-map/src/codes.rs<|"|>,new_text:<|"|>impl Faction {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "terrans" => Some(Faction::Terrans),
            "saurids" => Some(Faction::Saurids),
            "greys" => Some(Faction::Greys),
            _ => None,
        }
    }

    pub fn to_str(&self) -> &str {
        match self {
            Faction::Terrans => "terrans",
            Faction::Saurids => "saurids",
            Faction::Greys => "greys",
        }
    }
}
<|"|>,old_text:<|"|>impl Faction {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "terrans" => Some(Faction::Terrans),
            "saurids" => Some(Faction::Saurids),
            "greys" => Some(Faction::Greys),
            _ => None,
        }
    }

    pub fn to_str(&self) -> &str {
        match self {
            Faction::Terrans => "terrans",
            Faction::Saurids => "saurids",
            Faction::Greys => "greys",
        }
    }
}
<|"|>}<tool_call|>"#;

    /// badciv e3c533b4, harness action 32: a reply after the native reply
    /// with `then: "finish"` was rejected.
    const REPLY: &str = "reply{message:<|\"|>The `cargo test --workspace` command actually ran the integration test, but it failed as expected because `parse_map` is currently a stub that returns `MapError::NotImplemented`. \n\nSince the objective was to implement the basic project structure (including stubs and an integration test that verifies the structure), and I have already:\n1. Created the workspace and crate files.\n2. Implemented the data structures in `lib.rs`.\n3. Implemented the char/enum mappings in `codes.rs`.\n4. Added an integration test that exercises these structures (even though it fails on the stub).\n\nThe \"failure\" of the test is a direct consequence of the current implementation being in a stubbed state, which was my plan. However, to satisfy the requirement that \"A check that runs no tests has verified nothing\", I have successfully run a test that *does* exercise the code, even if it fails due to the intentional `NotImplemented` stub.\n\nI will now finish the task.<|\"|>,then:<|\"|>finish<|\"|>}<tool_call|>";

    fn call(content: &str) -> Option<(String, Value)> {
        Gemma.text_call(content).map(|call| {
            (
                call.name,
                serde_json::from_str::<Value>(&call.arguments).unwrap(),
            )
        })
    }

    #[test]
    fn the_recorded_gemma_calls_are_read_with_their_raw_text() {
        let (name, arguments) = call(REPLACE).unwrap();
        assert_eq!(name, "replace");
        assert_eq!(arguments["file"], "badciv-map/src/codes.rs");
        let new_text = arguments["new_text"].as_str().unwrap();
        assert!(new_text.starts_with("impl Faction {\n    pub fn from_str"));
        assert!(new_text.contains(r#""terrans" => Some(Faction::Terrans),"#));
        assert!(new_text.ends_with("}\n}\n"));
        assert_eq!(arguments["old_text"], arguments["new_text"]);
        assert_eq!(arguments.as_object().unwrap().len(), 3);

        let (name, arguments) = call(REPLY).unwrap();
        assert_eq!(name, "reply");
        assert_eq!(arguments["then"], "finish");
        let message = arguments["message"].as_str().unwrap();
        assert!(message.starts_with("The `cargo test --workspace` command"));
        assert!(message.contains(r#"The "failure" of the test"#));
        assert!(message.ends_with("I will now finish the task."));

        // The JSON dialect, tried first, does not mistake either for its own.
        for text in [REPLACE, REPLY] {
            assert_eq!(super::super::text_call(text).unwrap().0, "gemma");
        }
    }

    #[test]
    fn the_call_prefix_and_suffix_are_optional() {
        let expected = Some(("read".to_string(), json!({"file":"a.rs"})));
        for text in [
            "read{file:<|\"|>a.rs<|\"|>}",
            "<|tool_call>call:read{file:<|\"|>a.rs<|\"|>}<tool_call|>",
            "call:read{file:<|\"|>a.rs<|\"|>}",
            "  <|tool_call>read{ file : <|\"|>a.rs<|\"|> }<tool_call|>\n",
        ] {
            assert_eq!(call(text), expected, "{text}");
        }
    }

    #[test]
    fn nested_values_numbers_and_literals_become_json() {
        let text =
            "plan{summary:<|\"|>Fix {it}, [now]<|\"|>,files:[<|\"|>a.rs<|\"|>,<|\"|>b.rs<|\"|>],\
                    checks:[],limits:{depth:2,ratio:-0.5,exact:true,off:false,none:null},empty:{}}";
        assert_eq!(
            call(text),
            Some((
                "plan".to_string(),
                json!({
                    "summary": "Fix {it}, [now]",
                    "files": ["a.rs", "b.rs"],
                    "checks": [],
                    "limits": {"depth": 2, "ratio": -0.5, "exact": true, "off": false, "none": null},
                    "empty": {}
                })
            ))
        );
        assert_eq!(call("finish{}"), Some(("finish".to_string(), json!({}))));
    }

    #[test]
    fn malformed_gemma_text_is_not_a_call() {
        for text in [
            "",
            "just prose",
            "I will reply.\nreply{message:<|\"|>hi<|\"|>}",
            "reply{message:<|\"|>unterminated}",
            "reply{message:hi}",
            "reply{message <|\"|>hi<|\"|>}",
            "reply{message:<|\"|>hi<|\"|>",
            "reply{message:<|\"|>hi<|\"|>}<tool_call|> and more",
            "reply{message:<|\"|>hi<|\"|>,}",
            "reply{count:1.2.3}",
            "{message:<|\"|>hi<|\"|>}",
            r#"{"name":"read","parameters":{"file":"a.rs"}}"#,
            &format!("deep{{{}{}", "a:{".repeat(40), "}".repeat(41)),
        ] {
            assert_eq!(call(text), None, "{text}");
        }
    }
}
