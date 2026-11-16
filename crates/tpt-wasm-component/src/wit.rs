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

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::{ComponentFuncType, ComponentType, ComponentTypeDef, RecordField, Scalar};

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
    /// The records and enums the interface declares, in declaration order.
    pub types: Vec<WitTypeDef>,
}

/// A record or an enum an interface declares. Its fields and cases are written as WIT has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitTypeDef {
    /// `record name { field: type, ... }`, as `(field, type)` pairs.
    Record {
        name: String,
        fields: Vec<(String, String)>,
    },
    /// `enum name { case, ... }`, as case names.
    Enum { name: String, cases: Vec<String> },
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
    /// The document defines no interface of the name given.
    NoSuchInterface(String),
    /// The interface defines no function of the name given.
    NoSuchFunction { interface: String, function: String },
    /// A function's parameter or result is not a scalar type, which this mapping does not yet cover.
    UnsupportedType { function: String, ty: String },
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
            WitError::NoSuchInterface(interface) => {
                write!(f, "the document defines no interface {interface}")
            }
            WitError::NoSuchFunction {
                interface,
                function,
            } => write!(
                f,
                "the interface {interface} defines no function {function}"
            ),
            WitError::UnsupportedType { function, ty } => write!(
                f,
                "the function {function} has the type {ty}, which is not a scalar"
            ),
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

    /// The component types of `function` in `interface`, as a type table and a function type whose
    /// indices name that table. The table holds the interface's records and enums, in declaration
    /// order, followed by each list, option, tuple and result the signature uses. A string is allowed
    /// as a parameter or a result, but not inside a list, an option, a tuple, a result, or a record,
    /// which the component layer does not lay out. Anything else is refused by name.
    pub fn function_signature(
        &self,
        interface: &str,
        function: &str,
    ) -> Result<(Vec<ComponentTypeDef>, ComponentFuncType), WitError> {
        let found = self
            .interfaces
            .iter()
            .find(|candidate| candidate.name == interface)
            .ok_or_else(|| WitError::NoSuchInterface(interface.to_owned()))?;
        let definition = found
            .functions
            .iter()
            .find(|candidate| candidate.name == function)
            .ok_or_else(|| WitError::NoSuchFunction {
                interface: interface.to_owned(),
                function: function.to_owned(),
            })?;
        let mut table = SignatureTable {
            defs: Vec::new(),
            named: HashMap::new(),
            function,
        };
        for declared in &found.types {
            table.declare(declared)?;
        }
        let params = definition
            .params
            .iter()
            .map(|(_, ty)| table.value_type(ty))
            .collect::<Result<Vec<_>, _>>()?;
        let result = definition
            .result
            .as_deref()
            .map(|ty| table.value_type(ty))
            .transpose()?;
        Ok((table.defs, ComponentFuncType { params, result }))
    }

    /// The component function type of `function` in `interface`, for a function whose parameters
    /// and result are all scalars: `bool`, the integers of 8 to 64 bits, `f32`, `f64` and `char`.
    /// A string, a list or any other compound type is refused by name, since the component's
    /// type table for it is not built here.
    pub fn function_type(
        &self,
        interface: &str,
        function: &str,
    ) -> Result<ComponentFuncType, WitError> {
        let found = self
            .interfaces
            .iter()
            .find(|candidate| candidate.name == interface)
            .ok_or_else(|| WitError::NoSuchInterface(interface.to_owned()))?;
        let definition = found
            .functions
            .iter()
            .find(|candidate| candidate.name == function)
            .ok_or_else(|| WitError::NoSuchFunction {
                interface: interface.to_owned(),
                function: function.to_owned(),
            })?;
        let params = definition
            .params
            .iter()
            .map(|(_, ty)| scalar_type(ty, function))
            .collect::<Result<Vec<_>, _>>()?;
        let result = definition
            .result
            .as_deref()
            .map(|ty| scalar_type(ty, function))
            .transpose()?;
        Ok(ComponentFuncType { params, result })
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

/// A failure to read a WIT package directory from disk.
#[derive(Debug)]
pub enum WitLoadError {
    /// A file or directory could not be read.
    Io { path: PathBuf, message: String },
    /// A file was read but is not a WIT document this parser reads.
    Parse { path: PathBuf, error: WitError },
}

impl fmt::Display for WitLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WitLoadError::Io { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
            WitLoadError::Parse { path, error } => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for WitLoadError {}

/// Read a WIT package directory the way the WIT tooling lays one out: every `.wit` file directly
/// in `root`, then the packages under `root/deps`. A dependency is a directory of `.wit` files or a
/// single `.wit` file, and a dependency's own `deps` directory is read the same way.
///
/// Nothing is fetched. A package that is not on disk is not found, and a world that imports it is
/// refused by [`capability_ids_across`]. Entries are read in name order, so the result is the same
/// on every machine. The root's documents come first, then each dependency's, in that order.
pub fn load_directory(root: &Path) -> Result<Vec<WitDocument>, WitLoadError> {
    let mut documents = Vec::new();
    read_package_dir(root, &mut documents)?;
    Ok(documents)
}

fn read_package_dir(dir: &Path, out: &mut Vec<WitDocument>) -> Result<(), WitLoadError> {
    for entry in sorted_entries(dir)? {
        if entry.is_file() && is_wit_file(&entry) {
            out.push(parse_file(&entry)?);
        }
    }
    let deps = dir.join("deps");
    if deps.is_dir() {
        for entry in sorted_entries(&deps)? {
            if entry.is_dir() {
                read_package_dir(&entry, out)?;
            } else if entry.is_file() && is_wit_file(&entry) {
                out.push(parse_file(&entry)?);
            }
        }
    }
    Ok(())
}

fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>, WitLoadError> {
    let io = |message: String| WitLoadError::Io {
        path: dir.to_path_buf(),
        message,
    };
    let read = std::fs::read_dir(dir).map_err(|e| io(e.to_string()))?;
    let mut paths = Vec::new();
    for entry in read {
        paths.push(entry.map_err(|e| io(e.to_string()))?.path());
    }
    paths.sort();
    Ok(paths)
}

fn is_wit_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "wit")
}

fn parse_file(path: &Path) -> Result<WitDocument, WitLoadError> {
    let text = std::fs::read_to_string(path).map_err(|e| WitLoadError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    WitDocument::parse(&text).map_err(|error| WitLoadError::Parse {
        path: path.to_path_buf(),
        error,
    })
}

/// The type table a signature is built into, with the declared types by name.
struct SignatureTable<'a> {
    defs: Vec<ComponentTypeDef>,
    /// A declared record or enum, by name, as the index of its definition and whether it is a record.
    named: HashMap<String, (u32, bool)>,
    function: &'a str,
}

impl SignatureTable<'_> {
    fn refuse(&self, ty: &str) -> WitError {
        WitError::UnsupportedType {
            function: self.function.to_owned(),
            ty: ty.to_owned(),
        }
    }

    fn push(&mut self, def: ComponentTypeDef) -> u32 {
        self.defs.push(def);
        (self.defs.len() - 1) as u32
    }

    /// Add a declared record or enum. A record's fields must be scalars other than a string.
    fn declare(&mut self, declared: &WitTypeDef) -> Result<(), WitError> {
        match declared {
            WitTypeDef::Record { name, fields } => {
                let mut record = Vec::with_capacity(fields.len());
                for (field, ty) in fields {
                    let scalar = match scalar_name(ty) {
                        Some(Scalar::String) | None => return Err(self.refuse(ty)),
                        Some(scalar) => scalar,
                    };
                    record.push(RecordField {
                        name: field.clone(),
                        ty: scalar,
                    });
                }
                let index = self.push(ComponentTypeDef::Record(record));
                self.named.insert(name.clone(), (index, true));
            }
            WitTypeDef::Enum { name, cases } => {
                let index = self.push(ComponentTypeDef::Enum(cases.clone()));
                self.named.insert(name.clone(), (index, false));
            }
        }
        Ok(())
    }

    /// The component type a WIT value type names, adding any list, option, tuple or result it needs.
    fn value_type(&mut self, text: &str) -> Result<ComponentType, WitError> {
        let text = text.trim();
        if let Some(scalar) = scalar_name(text) {
            return Ok(ComponentType::Scalar(scalar));
        }
        if let Some((index, is_record)) = self.named.get(text).copied() {
            return Ok(if is_record {
                ComponentType::Record(index)
            } else {
                ComponentType::Enum(index)
            });
        }
        let Some((head, arguments)) = split_arguments(text) else {
            return Err(self.refuse(text));
        };
        match (head, arguments.as_slice()) {
            ("list", [element]) => {
                let element = self.payload(element)?;
                let index = self.push(ComponentTypeDef::List(element));
                Ok(ComponentType::List(index))
            }
            ("option", [payload]) => {
                let payload = self.payload(payload)?;
                let index = self.push(ComponentTypeDef::Option(payload));
                Ok(ComponentType::Option(index))
            }
            ("tuple", elements) if !elements.is_empty() => {
                let scalars = elements
                    .iter()
                    .map(|element| self.payload(element))
                    .collect::<Result<Vec<_>, _>>()?;
                let index = self.push(ComponentTypeDef::Tuple(scalars));
                Ok(ComponentType::Tuple(index))
            }
            ("result", [ok]) => {
                let ok = self.optional_payload(ok)?;
                let index = self.push(ComponentTypeDef::Result { ok, err: None });
                Ok(ComponentType::Result(index))
            }
            ("result", [ok, err]) => {
                let ok = self.optional_payload(ok)?;
                let err = self.optional_payload(err)?;
                let index = self.push(ComponentTypeDef::Result { ok, err });
                Ok(ComponentType::Result(index))
            }
            _ => Err(self.refuse(text)),
        }
    }

    /// A scalar that a list, option, tuple or result holds: any scalar but a string.
    fn payload(&self, text: &str) -> Result<Scalar, WitError> {
        match scalar_name(text.trim()) {
            Some(Scalar::String) | None => Err(self.refuse(text)),
            Some(scalar) => Ok(scalar),
        }
    }

    /// A result's payload, where `_` means the case has none.
    fn optional_payload(&self, text: &str) -> Result<Option<Scalar>, WitError> {
        if text.trim() == "_" {
            return Ok(None);
        }
        self.payload(text).map(Some)
    }
}

/// The scalar a WIT type name is, including `string`, or `None` if it names something else.
fn scalar_name(text: &str) -> Option<Scalar> {
    Some(match text.trim() {
        "bool" => Scalar::Bool,
        "s8" => Scalar::S8,
        "u8" => Scalar::U8,
        "s16" => Scalar::S16,
        "u16" => Scalar::U16,
        "s32" => Scalar::S32,
        "u32" => Scalar::U32,
        "s64" => Scalar::S64,
        "u64" => Scalar::U64,
        "f32" => Scalar::F32,
        "f64" => Scalar::F64,
        "char" => Scalar::Char,
        "string" => Scalar::String,
        _ => return None,
    })
}

/// Split `head<a, b<c>, d>` into its head and its top-level arguments. `None` if the text is not a
/// name followed by a bracketed list.
fn split_arguments(text: &str) -> Option<(&str, Vec<&str>)> {
    let open = text.find('<')?;
    if !text.ends_with('>') {
        return None;
    }
    let head = text[..open].trim();
    let inner = &text[open + 1..text.len() - 1];
    let mut arguments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (position, character) in inner.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                arguments.push(inner[start..position].trim());
                start = position + 1;
            }
            _ => {}
        }
    }
    arguments.push(inner[start..].trim());
    if depth != 0 || arguments.iter().any(|argument| argument.is_empty()) {
        return None;
    }
    Some((head, arguments))
}

/// The component type of a WIT scalar type name, or a refusal naming the function that has it.
fn scalar_type(ty: &str, function: &str) -> Result<ComponentType, WitError> {
    let scalar = match ty.trim() {
        "bool" => Scalar::Bool,
        "s8" => Scalar::S8,
        "u8" => Scalar::U8,
        "s16" => Scalar::S16,
        "u16" => Scalar::U16,
        "s32" => Scalar::S32,
        "u32" => Scalar::U32,
        "s64" => Scalar::S64,
        "u64" => Scalar::U64,
        "f32" => Scalar::F32,
        "f64" => Scalar::F64,
        "char" => Scalar::Char,
        other => {
            return Err(WitError::UnsupportedType {
                function: function.to_owned(),
                ty: other.to_owned(),
            })
        }
    };
    Ok(ComponentType::Scalar(scalar))
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
        let mut types = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Token::Symbol('}')) => {
                    self.next();
                    break;
                }
                Some(Token::Ident(word)) if word == "record" => {
                    types.push(self.record_definition()?)
                }
                Some(Token::Ident(word)) if word == "enum" => types.push(self.enum_definition()?),
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
        Ok(WitInterface {
            name,
            functions,
            types,
        })
    }

    /// `record name { field: type, ... }`, with an optional comma after each field.
    fn record_definition(&mut self) -> Result<WitTypeDef, WitError> {
        self.next();
        let name = self.expect_ident()?;
        self.expect_symbol('{')?;
        let mut fields = Vec::new();
        loop {
            if self.peek() == Some(&Token::Symbol('}')) {
                self.next();
                break;
            }
            let field = self.expect_ident()?;
            self.expect_symbol(':')?;
            let ty = self.type_text()?;
            fields.push((field, ty));
            match self.peek() {
                Some(Token::Symbol(',')) => {
                    self.next();
                }
                Some(Token::Symbol('}')) => {}
                _ => return Err(self.error("',' or '}' in a record")),
            }
        }
        Ok(WitTypeDef::Record { name, fields })
    }

    /// `enum name { case, ... }`, with an optional comma after each case.
    fn enum_definition(&mut self) -> Result<WitTypeDef, WitError> {
        self.next();
        let name = self.expect_ident()?;
        self.expect_symbol('{')?;
        let mut cases = Vec::new();
        loop {
            if self.peek() == Some(&Token::Symbol('}')) {
                self.next();
                break;
            }
            cases.push(self.expect_ident()?);
            match self.peek() {
                Some(Token::Symbol(',')) => {
                    self.next();
                }
                Some(Token::Symbol('}')) => {}
                _ => return Err(self.error("',' or '}' in an enum")),
            }
        }
        Ok(WitTypeDef::Enum { name, cases })
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

    const MATH: &str = "package demo:math@1.0.0;

interface ops {
    add: func(a: s32, b: s32) -> s32;
    flag: func(on: bool) -> char;
    greet: func(name: string);
}

world w {
    import ops;
}
";

    #[test]
    fn a_scalar_signature_maps_to_a_component_function_type() {
        let document = WitDocument::parse(MATH).expect("parses");
        assert_eq!(
            document.function_type("ops", "add"),
            Ok(ComponentFuncType {
                params: vec![
                    ComponentType::Scalar(Scalar::S32),
                    ComponentType::Scalar(Scalar::S32),
                ],
                result: Some(ComponentType::Scalar(Scalar::S32)),
            })
        );
        assert_eq!(
            document.function_type("ops", "flag"),
            Ok(ComponentFuncType {
                params: vec![ComponentType::Scalar(Scalar::Bool)],
                result: Some(ComponentType::Scalar(Scalar::Char)),
            })
        );
    }

    #[test]
    fn a_string_parameter_and_a_missing_name_are_refused_by_name() {
        let document = WitDocument::parse(MATH).expect("parses");
        assert_eq!(
            document.function_type("ops", "greet"),
            Err(WitError::UnsupportedType {
                function: "greet".to_owned(),
                ty: "string".to_owned(),
            })
        );
        assert_eq!(
            document.function_type("nope", "add"),
            Err(WitError::NoSuchInterface("nope".to_owned()))
        );
        assert_eq!(
            document.function_type("ops", "nope"),
            Err(WitError::NoSuchFunction {
                interface: "ops".to_owned(),
                function: "nope".to_owned(),
            })
        );
    }

    /// A scratch directory for one test, cleared before use.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tpt-wit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(path, text).expect("a file");
    }

    #[test]
    fn a_package_directory_resolves_its_dependencies_from_deps() {
        let root = scratch("deps");
        write(
            &root.join("app.wit"),
            "package local:app;
world w { import wasi:io/poll@0.2.0; }
",
        );
        write(
            &root.join("deps/wasi-io/poll.wit"),
            "package wasi:io@0.2.0;
interface poll { ready: func() -> bool; }
",
        );
        let documents = load_directory(&root).expect("loads");
        assert_eq!(documents.len(), 2, "the root's file, then the dependency");
        assert_eq!(
            capability_ids_across(&documents, 0, "w"),
            Ok(vec!["wasi:io/poll@0.2.0#ready".to_owned()])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_dependency_is_not_fetched_and_is_refused_by_name() {
        let root = scratch("missing");
        write(
            &root.join("app.wit"),
            "package local:app;
world w { import wasi:io/poll@0.2.0; }
",
        );
        let documents = load_directory(&root).expect("loads");
        assert!(matches!(
            capability_ids_across(&documents, 0, "w"),
            Err(WitError::UnresolvedInterface { .. })
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_file_is_reported_with_its_path() {
        let root = scratch("malformed");
        write(
            &root.join("bad.wit"),
            "package a:b;
package c:d;
",
        );
        match load_directory(&root) {
            Err(WitLoadError::Parse { path, error }) => {
                assert!(path.ends_with("bad.wit"));
                assert!(matches!(error, WitError::Syntax { line: 2, .. }));
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    const SHAPES: &str = "\
package demo:shapes@1.0.0;

interface geometry {
    record point {
        x: s32,
        y: s32,
    }

    enum color { red, green, blue }

    area: func(p: point, c: color) -> u64;
    points: func(xs: list<u8>, label: string) -> tuple<u8, bool>;
    maybe: func(v: u32) -> option<u32>;
    checked: func(v: u32) -> result<u32, u8>;
    empty: func() -> result<_, u8>;
    corners: func(p: point) -> list<point>;
    named: func(s: string) -> list<string>;
}

world w {
    import geometry;
}
";

    #[test]
    fn a_record_and_an_enum_map_to_their_own_definitions() {
        let document = WitDocument::parse(SHAPES).expect("parses");
        let (table, signature) = document
            .function_signature("geometry", "area")
            .expect("maps");
        assert_eq!(
            table,
            vec![
                ComponentTypeDef::Record(vec![
                    RecordField {
                        name: "x".to_owned(),
                        ty: Scalar::S32,
                    },
                    RecordField {
                        name: "y".to_owned(),
                        ty: Scalar::S32,
                    },
                ]),
                ComponentTypeDef::Enum(vec![
                    "red".to_owned(),
                    "green".to_owned(),
                    "blue".to_owned(),
                ]),
            ]
        );
        assert_eq!(
            signature,
            ComponentFuncType {
                params: vec![ComponentType::Record(0), ComponentType::Enum(1)],
                result: Some(ComponentType::Scalar(Scalar::U64)),
            }
        );
    }

    #[test]
    fn lists_options_tuples_and_results_are_added_to_the_table_after_the_declarations() {
        let document = WitDocument::parse(SHAPES).expect("parses");
        let (table, signature) = document
            .function_signature("geometry", "points")
            .expect("maps");
        assert_eq!(table[2], ComponentTypeDef::List(Scalar::U8));
        assert_eq!(
            table[3],
            ComponentTypeDef::Tuple(vec![Scalar::U8, Scalar::Bool])
        );
        assert_eq!(
            signature,
            ComponentFuncType {
                params: vec![
                    ComponentType::List(2),
                    ComponentType::Scalar(Scalar::String)
                ],
                result: Some(ComponentType::Tuple(3)),
            }
        );

        let (table, signature) = document
            .function_signature("geometry", "maybe")
            .expect("maps");
        assert_eq!(
            table,
            vec![
                ComponentTypeDef::Record(vec![
                    RecordField {
                        name: "x".to_owned(),
                        ty: Scalar::S32
                    },
                    RecordField {
                        name: "y".to_owned(),
                        ty: Scalar::S32
                    },
                ]),
                ComponentTypeDef::Enum(vec![
                    "red".to_owned(),
                    "green".to_owned(),
                    "blue".to_owned()
                ]),
                ComponentTypeDef::Option(Scalar::U32),
            ]
        );
        assert_eq!(signature.result, Some(ComponentType::Option(2)));

        let (table, signature) = document
            .function_signature("geometry", "checked")
            .expect("maps");
        assert_eq!(
            table[2],
            ComponentTypeDef::Result {
                ok: Some(Scalar::U32),
                err: Some(Scalar::U8),
            }
        );
        assert_eq!(signature.result, Some(ComponentType::Result(2)));

        let (table, signature) = document
            .function_signature("geometry", "empty")
            .expect("maps");
        assert_eq!(
            table[2],
            ComponentTypeDef::Result {
                ok: None,
                err: Some(Scalar::U8),
            }
        );
        assert!(signature.params.is_empty());
    }

    #[test]
    fn compound_shapes_the_component_layer_does_not_lay_out_are_refused_by_name() {
        let document = WitDocument::parse(SHAPES).expect("parses");
        assert_eq!(
            document.function_signature("geometry", "corners"),
            Err(WitError::UnsupportedType {
                function: "corners".to_owned(),
                ty: "point".to_owned(),
            }),
            "a list of records is not laid out"
        );
        assert_eq!(
            document.function_signature("geometry", "named"),
            Err(WitError::UnsupportedType {
                function: "named".to_owned(),
                ty: "string".to_owned(),
            }),
            "a list of strings is not laid out"
        );
    }

    #[test]
    fn a_record_with_a_string_field_is_refused_at_its_declaration() {
        let document = WitDocument::parse(
            "package a:b;\ninterface i {\n  record r { name: string }\n  f: func(x: r);\n}\n",
        )
        .expect("parses");
        assert!(matches!(
            document.function_signature("i", "f"),
            Err(WitError::UnsupportedType { ty, .. }) if ty == "string"
        ));
    }
}
