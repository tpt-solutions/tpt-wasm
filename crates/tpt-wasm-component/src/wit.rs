//! A parser for the part of WIT that names what a world imports, and the capability ids those
//! imports map to.
//!
//! WIT is the interface language of the Component Model. This module reads one document: its
//! package declaration, its interfaces and their functions, and its worlds. Only what a world
//! imports matters for capabilities, so the rest is read past rather than kept: type definitions,
//! `use`, `include`, and exports. A function import named by a plain name is its own capability,
//! with that name as its id. An interface import gives one capability for each of its functions,
//! named `namespace:package/interface@version#function`, the interface's name as a component
//! import carries it, with the function after `#`. That `#` form is this crate's convention, since
//! the Component Model names an interface import as a whole instance.
//!
//! An interface a world imports is found in the document, or, given several documents, in the one
//! whose package has the path's name and version. An interface found in none is refused by name.
//! Comments are `//` and nested `/* */`, as WIT has them.

use std::fmt;

/// One parsed WIT document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitDocument {
    /// The package the document declares, if it declares one.
    pub package: Option<PackageName>,
    /// The interfaces the document defines, in order.
    pub interfaces: Vec<WitInterface>,
    /// The worlds the document defines, in order.
    pub worlds: Vec<WitWorld>,
}

/// A package name such as `wasi:clocks@0.2.0`: a namespace, the package path after it, and a
/// version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageName {
    /// The namespace, the first segment of the name.
    pub namespace: String,
    /// The package segments after the namespace; a plain package has one.
    pub packages: Vec<String>,
    /// The version, as written after `@`.
    pub version: Option<String>,
}

impl PackageName {
    /// The package without its version, as `namespace:package`.
    pub fn unversioned(&self) -> String {
        let mut name = self.namespace.clone();
        for segment in &self.packages {
            name.push(':');
            name.push_str(segment);
        }
        name
    }
}

/// An interface and the functions it declares. Types it declares are read past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitInterface {
    /// The interface's name.
    pub name: String,
    /// The functions the interface declares, in order.
    pub functions: Vec<WitFunction>,
}

/// A function: its name, its named parameters, and its result, each type written as WIT has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitFunction {
    /// The function's name.
    pub name: String,
    /// The parameters, as `(name, type)` pairs.
    pub params: Vec<(String, String)>,
    /// The result type, if the function returns one.
    pub result: Option<String>,
}

/// A world and what it imports. Its exports are read past, since they are not capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitWorld {
    /// The world's name.
    pub name: String,
    /// The imports, in order.
    pub imports: Vec<WitImport>,
}

/// One import of a world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitImport {
    /// A function imported by its own name.
    Function(WitFunction),
    /// An interface, named as the world writes it: a name the document defines, or a path such
    /// as `wasi:clocks/monotonic-clock`, which the document does not define.
    Interface(String),
}

/// A failure to read a document, or to map a world's imports to capability ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitError {
    /// The text is not WIT that this parser reads. The line is where the problem was found.
    Syntax { line: usize, message: String },
    /// The world named is not defined by the document.
    NoSuchWorld(String),
    /// A world imports an interface the document does not define.
    UnresolvedInterface { world: String, interface: String },
    /// An interface's functions need the package to name them, and the document has none.
    MissingPackage,
}

impl fmt::Display for WitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WitError::Syntax { line, message } => {
                write!(f, "line {line}: expected {message}")
            }
            WitError::NoSuchWorld(world) => write!(f, "the document defines no world {world}"),
            WitError::UnresolvedInterface { world, interface } => write!(
                f,
                "the world {world} imports {interface}, which the document does not define"
            ),
            WitError::MissingPackage => {
                write!(f, "the interface's functions need a package declaration")
            }
        }
    }
}

impl std::error::Error for WitError {}

impl WitDocument {
    /// Read a WIT document.
    pub fn parse(source: &str) -> Result<Self, WitError> {
        let mut parser = Parser {
            tokens: tokenize(source)?,
            position: 0,
        };
        let package = if parser.keyword("package") {
            Some(parser.package_declaration()?)
        } else {
            None
        };
        let mut interfaces = Vec::new();
        let mut worlds = Vec::new();
        while let Some(token) = parser.peek().cloned() {
            match token {
                Token::Ident(word) if word == "interface" => interfaces.push(parser.interface()?),
                Token::Ident(word) if word == "world" => worlds.push(parser.world()?),
                Token::Ident(word) if word == "use" => parser.skip_item()?,
                _ => return Err(parser.error("an interface, a world, or a use at the top level")),
            }
        }
        Ok(WitDocument {
            package,
            interfaces,
            worlds,
        })
    }

    /// The capability ids of a world's imports, in the order the world lists them. A function
    /// import gives its own name. An interface import gives one id for each of its functions.
    pub fn capability_ids(&self, world: &str) -> Result<Vec<String>, WitError> {
        capability_ids_across(std::slice::from_ref(self), 0, world)
    }

    /// The name a component import gives an interface of this document: `namespace:package/name`,
    /// with the version after the name when the package has one.
    fn interface_name(&self, name: &str) -> Result<String, WitError> {
        let package = self.package.as_ref().ok_or(WitError::MissingPackage)?;
        let mut full = format!("{}/{name}", package.unversioned());
        if let Some(version) = &package.version {
            full.push('@');
            full.push_str(version);
        }
        Ok(full)
    }
}

/// The capability ids of the world named `world` in `documents[index]`. An interface the world
/// imports is looked up by the document's own names, or, when the import is a path, in the
/// document whose package has that name and version.
pub fn capability_ids_across(
    documents: &[WitDocument],
    index: usize,
    world: &str,
) -> Result<Vec<String>, WitError> {
    let home = documents
        .get(index)
        .ok_or_else(|| WitError::NoSuchWorld(world.to_owned()))?;
    let world_def = home
        .worlds
        .iter()
        .find(|candidate| candidate.name == world)
        .ok_or_else(|| WitError::NoSuchWorld(world.to_owned()))?;
    let mut ids = Vec::new();
    for import in &world_def.imports {
        match import {
            WitImport::Function(function) => ids.push(function.name.clone()),
            WitImport::Interface(path) => {
                let (owner, interface) =
                    find_interface(documents, index, path).ok_or_else(|| {
                        WitError::UnresolvedInterface {
                            world: world_def.name.clone(),
                            interface: path.clone(),
                        }
                    })?;
                let prefix = owner.interface_name(&interface.name)?;
                for function in &interface.functions {
                    ids.push(format!("{prefix}#{}", function.name));
                }
            }
        }
    }
    Ok(ids)
}

/// The document and the interface a world's import names. A plain name is an interface of the
/// document at `home`. A path `namespace:package/interface@version` is an interface of the document
/// whose package has that name, and that version when the path gives one.
fn find_interface<'a>(
    documents: &'a [WitDocument],
    home: usize,
    path: &str,
) -> Option<(&'a WitDocument, &'a WitInterface)> {
    let Some((package_path, rest)) = path.split_once('/') else {
        let document = documents.get(home)?;
        let interface = document
            .interfaces
            .iter()
            .find(|candidate| candidate.name == path)?;
        return Some((document, interface));
    };
    let (interface_name, version) = match rest.split_once('@') {
        Some((name, version)) => (name, Some(version)),
        None => (rest, None),
    };
    documents.iter().find_map(|document| {
        let package = document.package.as_ref()?;
        if package.unversioned() != package_path {
            return None;
        }
        if let Some(wanted) = version {
            if package.version.as_deref() != Some(wanted) {
                return None;
            }
        }
        let interface = document
            .interfaces
            .iter()
            .find(|candidate| candidate.name == interface_name)?;
        Some((document, interface))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Symbol(char),
    Arrow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Spanned {
    token: Token,
    line: usize,
}

fn syntax(line: usize, message: impl Into<String>) -> WitError {
    WitError::Syntax {
        line,
        message: message.into(),
    }
}

/// Split WIT text into tokens, skipping whitespace and comments. An identifier may hold the
/// letters, digits, `-`, `_`, `.`, and `+` of a name or of a version.
fn tokenize(source: &str) -> Result<Vec<Spanned>, WitError> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut line = 1;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            let mut depth = 0usize;
            loop {
                if i >= chars.len() {
                    return Err(syntax(line, "the end of a block comment"));
                }
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if c == '-' && chars.get(i + 1) == Some(&'>') {
            tokens.push(Spanned {
                token: Token::Arrow,
                line,
            });
            i += 2;
        } else if c.is_ascii_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '-' | '_' | '.' | '+'))
            {
                i += 1;
            }
            tokens.push(Spanned {
                token: Token::Ident(chars[start..i].iter().collect()),
                line,
            });
        } else if "{}()<>:;,=/@".contains(c) {
            tokens.push(Spanned {
                token: Token::Symbol(c),
                line,
            });
            i += 1;
        } else {
            return Err(syntax(line, format!("a WIT token, not {c:?}")));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Spanned>,
    position: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.peek_at(0)
    }

    fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.tokens
            .get(self.position + offset)
            .map(|spanned| &spanned.token)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.peek().cloned();
        if token.is_some() {
            self.position += 1;
        }
        token
    }

    fn line(&self) -> usize {
        self.tokens
            .get(self.position)
            .or(self.tokens.last())
            .map_or(1, |spanned| spanned.line)
    }

    fn error(&self, expected: &str) -> WitError {
        syntax(self.line(), expected)
    }

    fn keyword(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(found)) if found == word)
    }

    fn expect_symbol(&mut self, symbol: char) -> Result<(), WitError> {
        match self.next() {
            Some(Token::Symbol(found)) if found == symbol => Ok(()),
            _ => Err(self.error(&format!("{symbol:?}"))),
        }
    }

    fn expect_ident(&mut self) -> Result<String, WitError> {
        match self.next() {
            Some(Token::Ident(name)) => Ok(name),
            _ => Err(self.error("a name")),
        }
    }

    /// `package namespace:package(:segment)*(@version)?;`, with a path after `/` refused.
    fn package_declaration(&mut self) -> Result<PackageName, WitError> {
        self.next();
        let mut segments = vec![self.expect_ident()?];
        while self.peek() == Some(&Token::Symbol(':')) {
            self.next();
            segments.push(self.expect_ident()?);
        }
        if self.peek() == Some(&Token::Symbol('/')) {
            return Err(self.error("a package name without a path after '/'"));
        }
        let mut version = None;
        if self.peek() == Some(&Token::Symbol('@')) {
            self.next();
            version = Some(self.expect_ident()?);
        }
        self.expect_symbol(';')?;
        if segments.len() < 2 {
            return Err(self.error("a namespace and a package in the package name"));
        }
        let namespace = segments.remove(0);
        Ok(PackageName {
            namespace,
            packages: segments,
            version,
        })
    }

    fn interface(&mut self) -> Result<WitInterface, WitError> {
        self.next();
        let name = self.expect_ident()?;
        self.expect_symbol('{')?;
        let mut functions = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Token::Symbol('}')) => {
                    self.next();
                    break;
                }
                Some(Token::Ident(word))
                    if matches!(
                        word.as_str(),
                        "use" | "type" | "record" | "variant" | "enum" | "flags" | "resource"
                    ) =>
                {
                    self.skip_item()?
                }
                Some(Token::Ident(_)) if self.peek_at(1) == Some(&Token::Symbol(':')) => {
                    functions.push(self.function()?)
                }
                _ => return Err(self.error("a function, a type, a use, or '}'")),
            }
        }
        Ok(WitInterface { name, functions })
    }

    fn world(&mut self) -> Result<WitWorld, WitError> {
        self.next();
        let name = self.expect_ident()?;
        self.expect_symbol('{')?;
        let mut imports = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Token::Symbol('}')) => {
                    self.next();
                    break;
                }
                Some(Token::Ident(word)) if word == "import" => {
                    self.next();
                    if self.is_function_declaration() {
                        imports.push(WitImport::Function(self.function()?));
                    } else {
                        imports.push(WitImport::Interface(self.path()?));
                    }
                }
                Some(Token::Ident(word))
                    if matches!(
                        word.as_str(),
                        "export"
                            | "include"
                            | "use"
                            | "type"
                            | "record"
                            | "variant"
                            | "enum"
                            | "flags"
                            | "resource"
                    ) =>
                {
                    self.skip_item()?
                }
                _ => return Err(self.error("an import, an export, or '}'")),
            }
        }
        Ok(WitWorld { name, imports })
    }

    /// Whether the next tokens start a function declaration: a name, a colon, and `func`.
    fn is_function_declaration(&self) -> bool {
        matches!(self.peek(), Some(Token::Ident(_)))
            && self.peek_at(1) == Some(&Token::Symbol(':'))
            && matches!(self.peek_at(2), Some(Token::Ident(word)) if word == "func")
    }

    /// A name or a path such as `namespace:package/interface@version`, up to the `;` that ends it.
    fn path(&mut self) -> Result<String, WitError> {
        let mut text = String::new();
        loop {
            match self.next() {
                Some(Token::Symbol(';')) => break,
                Some(Token::Ident(part)) => text.push_str(&part),
                Some(Token::Symbol(symbol)) if matches!(symbol, ':' | '/' | '@') => {
                    text.push(symbol)
                }
                _ => return Err(self.error("an interface path ending in ';'")),
            }
        }
        if text.is_empty() {
            return Err(self.error("an interface name"));
        }
        Ok(text)
    }

    /// `name: func(params) (-> type)?;`. A static or an async function is refused.
    fn function(&mut self) -> Result<WitFunction, WitError> {
        let name = self.expect_ident()?;
        self.expect_symbol(':')?;
        if self.keyword("async") || self.keyword("static") {
            return Err(self.error("a plain func, not a static or async one"));
        }
        if !self.keyword("func") {
            return Err(self.error("func"));
        }
        self.next();
        self.expect_symbol('(')?;
        let mut params = Vec::new();
        loop {
            if self.peek() == Some(&Token::Symbol(')')) {
                self.next();
                break;
            }
            let param = self.expect_ident()?;
            self.expect_symbol(':')?;
            let ty = self.type_text()?;
            params.push((param, ty));
            match self.next() {
                Some(Token::Symbol(',')) => {}
                Some(Token::Symbol(')')) => break,
                _ => return Err(self.error("',' or ')' after a parameter")),
            }
        }
        let result = if self.peek() == Some(&Token::Arrow) {
            self.next();
            Some(self.type_text()?)
        } else {
            None
        };
        self.expect_symbol(';')?;
        Ok(WitFunction {
            name,
            params,
            result,
        })
    }

    /// A type: a name, or a name with type arguments in angle brackets, as `list<u8>`.
    fn type_text(&mut self) -> Result<String, WitError> {
        let name = self.expect_ident()?;
        if self.peek() != Some(&Token::Symbol('<')) {
            return Ok(name);
        }
        self.next();
        let mut arguments = Vec::new();
        loop {
            arguments.push(self.type_text()?);
            match self.next() {
                Some(Token::Symbol(',')) => {}
                Some(Token::Symbol('>')) => break,
                _ => return Err(self.error("',' or '>' in a type")),
            }
        }
        Ok(format!("{name}<{}>", arguments.join(", ")))
    }

    /// Read past an item this parser does not keep. It ends at a `;` outside braces, or at the
    /// brace that closes a block, with a `;` after that block taken too if there is one.
    fn skip_item(&mut self) -> Result<(), WitError> {
        let mut depth = 0usize;
        loop {
            match self.next() {
                None => return Err(self.error("the end of an item")),
                Some(Token::Symbol('{')) => depth += 1,
                Some(Token::Symbol('}')) => {
                    if depth == 0 {
                        return Err(self.error("an item before '}'"));
                    }
                    depth -= 1;
                    if depth == 0 {
                        if self.peek() == Some(&Token::Symbol(';')) {
                            self.next();
                        }
                        return Ok(());
                    }
                }
                Some(Token::Symbol(';')) if depth == 0 => return Ok(()),
                Some(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOCKS: &str = "\
package wasi:clocks@0.2.0;

interface monotonic-clock {
    use wasi:io/poll.{pollable};

    // The current instant, in nanoseconds.
    now: func() -> u64;
    resolution: func() -> u64;
    subscribe: func(when: u64) -> pollable;
}

world imports {
    import monotonic-clock;
    import log: func(message: string);
    export run: func() -> result<_, string>;
}
";

    #[test]
    fn a_document_yields_its_package_interfaces_and_worlds() {
        let document = WitDocument::parse(CLOCKS).expect("parses");
        let package = document.package.expect("a package");
        assert_eq!(package.unversioned(), "wasi:clocks");
        assert_eq!(package.version.as_deref(), Some("0.2.0"));
        assert_eq!(document.interfaces.len(), 1);
        let clock = &document.interfaces[0];
        assert_eq!(clock.name, "monotonic-clock");
        let names: Vec<&str> = clock.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["now", "resolution", "subscribe"]);
        assert_eq!(
            clock.functions[2].params,
            [("when".to_owned(), "u64".to_owned())]
        );
        assert_eq!(clock.functions[2].result.as_deref(), Some("pollable"));
        assert_eq!(document.worlds[0].name, "imports");
    }

    #[test]
    fn a_world_maps_its_imports_to_capability_ids() {
        let document = WitDocument::parse(CLOCKS).expect("parses");
        // The interface's functions take its versioned name, and a plain function its own name.
        // The export is not an import, so it is not a capability.
        assert_eq!(
            document.capability_ids("imports"),
            Ok(vec![
                "wasi:clocks/monotonic-clock@0.2.0#now".to_owned(),
                "wasi:clocks/monotonic-clock@0.2.0#resolution".to_owned(),
                "wasi:clocks/monotonic-clock@0.2.0#subscribe".to_owned(),
                "log".to_owned(),
            ])
        );
    }

    #[test]
    fn an_unversioned_package_names_its_interfaces_without_a_version() {
        let document = WitDocument::parse(
            "package local:demo;\ninterface foo { a: func(); }\nworld w { import foo; }\n",
        )
        .expect("parses");
        assert_eq!(
            document.capability_ids("w"),
            Ok(vec!["local:demo/foo#a".to_owned()])
        );
    }

    #[test]
    fn an_interface_of_another_document_is_found_by_its_package_path() {
        let wasi = WitDocument::parse(
            "package wasi:io@0.2.0;\ninterface poll { ready: func() -> bool; }\n",
        )
        .expect("parses");
        let app = WitDocument::parse(
            "package local:app;\nworld w { import wasi:io/poll@0.2.0; import ping: func(); }\n",
        )
        .expect("parses");
        let documents = [app.clone(), wasi.clone()];
        assert_eq!(
            capability_ids_across(&documents, 0, "w"),
            Ok(vec![
                "wasi:io/poll@0.2.0#ready".to_owned(),
                "ping".to_owned(),
            ])
        );
        // A version the other package does not have does not resolve.
        let wrong =
            WitDocument::parse("package local:app;\nworld w { import wasi:io/poll@0.1.0; }\n")
                .expect("parses");
        assert!(matches!(
            capability_ids_across(&[wrong, wasi.clone()], 0, "w"),
            Err(WitError::UnresolvedInterface { .. })
        ));
        // A path without a version finds the package whatever its version.
        let unversioned =
            WitDocument::parse("package local:app;\nworld w { import wasi:io/poll; }\n")
                .expect("parses");
        assert_eq!(
            capability_ids_across(&[unversioned, wasi], 0, "w"),
            Ok(vec!["wasi:io/poll@0.2.0#ready".to_owned()])
        );
    }

    #[test]
    fn an_interface_from_outside_the_document_is_refused_by_name() {
        let document =
            WitDocument::parse("package local:demo;\nworld w { import wasi:io/poll@0.2.0; }\n")
                .expect("parses");
        assert_eq!(
            document.capability_ids("w"),
            Err(WitError::UnresolvedInterface {
                world: "w".to_owned(),
                interface: "wasi:io/poll@0.2.0".to_owned(),
            })
        );
    }

    #[test]
    fn a_missing_world_and_a_package_less_interface_are_refused() {
        let document =
            WitDocument::parse("interface foo { a: func(); }\nworld w { import foo; }\n")
                .expect("parses");
        assert_eq!(
            document.capability_ids("other"),
            Err(WitError::NoSuchWorld("other".to_owned()))
        );
        assert_eq!(document.capability_ids("w"), Err(WitError::MissingPackage));
    }

    #[test]
    fn nested_comments_and_record_bodies_are_read_past() {
        let document = WitDocument::parse(
            "package a:b;\n/* outer /* inner */ still a comment */\ninterface i {\n    record point { x: u32, y: u32 }\n    variant shape { circle(u32), square }\n    f: func(p: point) -> option<point>;\n}\nworld w { import i; }\n",
        )
        .expect("parses");
        assert_eq!(document.capability_ids("w"), Ok(vec!["a:b/i#f".to_owned()]));
    }

    #[test]
    fn a_syntax_error_names_its_line() {
        let error = WitDocument::parse("package a:b;\ninterface i {\n    f: func(x u32);\n}\n")
            .expect_err("refused");
        assert!(matches!(error, WitError::Syntax { line: 3, .. }));
    }

    #[test]
    fn static_and_async_functions_are_refused() {
        assert!(matches!(
            WitDocument::parse("package a:b;\ninterface i { f: async func(); }\n"),
            Err(WitError::Syntax { line: 2, .. })
        ));
    }

    #[test]
    fn a_second_package_is_refused() {
        assert!(matches!(
            WitDocument::parse("package a:b;\npackage c:d;\n"),
            Err(WitError::Syntax { line: 2, .. })
        ));
    }
}
