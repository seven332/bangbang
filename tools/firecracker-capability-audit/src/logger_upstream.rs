use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Expr, ForeignItem, ImplItem, Item, Meta, Stmt, Token, TraitItem};

use crate::upstream::{ensure_regular_input, git_output, read_input};
use crate::{
    AuditError, Baseline, FIRECRACKER_COMMIT, FIRECRACKER_TARGET, FIRECRACKER_VERSION, Input,
    LOGGER_PRODUCER_GENERATOR_VERSION, LOGGER_PRODUCER_SCHEMA_VERSION, LoggerInvocation,
    LoggerInvocationSyntax, LoggerMacro, LoggerProducerCounts, LoggerProducerManifest,
    LoggerSourceContext, ensure_pinned_checkout,
};

const LOGGER_EXTRACTOR: &str = "rust-logger-macro-v1";

/// Derive the machine-owned logger producer manifest from pinned Rust syntax.
pub fn derive_logger_producer_manifest(path: &Path) -> Result<LoggerProducerManifest, AuditError> {
    let checkout = ensure_pinned_checkout(path)?;
    let rust_paths = tracked_rust_paths(&checkout)?;
    let mut invocations = Vec::new();
    let mut inputs = Vec::new();

    for input_path in &rust_paths {
        let source = read_input(&checkout, input_path)?;
        let mut file_invocations = extract_logger_invocations(input_path, &source)?;
        if file_invocations.is_empty() {
            continue;
        }
        let object = format!("HEAD:{input_path}");
        inputs.push(Input {
            path: input_path.clone(),
            git_blob: git_output(&checkout, &["rev-parse", &object])?,
            extractor: LOGGER_EXTRACTOR.to_string(),
        });
        invocations.append(&mut file_invocations);
    }

    invocations.sort_by(|left, right| left.id.cmp(&right.id));
    let counts = logger_counts(rust_paths.len(), inputs.len(), &invocations);
    Ok(LoggerProducerManifest {
        schema_version: LOGGER_PRODUCER_SCHEMA_VERSION,
        baseline: Baseline {
            version: FIRECRACKER_VERSION.to_string(),
            commit: FIRECRACKER_COMMIT.to_string(),
            target: FIRECRACKER_TARGET.to_string(),
        },
        generator_version: LOGGER_PRODUCER_GENERATOR_VERSION,
        inputs,
        counts,
        invocations,
    })
}

fn tracked_rust_paths(checkout: &Path) -> Result<Vec<String>, AuditError> {
    let paths = git_output(
        checkout,
        &["ls-tree", "-r", "--name-only", "HEAD", "--", "src"],
    )?;
    let rust_paths = paths
        .lines()
        .filter(|path| path.starts_with("src/") && path.ends_with(".rs"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    for path in &rust_paths {
        ensure_regular_input(checkout, path)?;
    }
    Ok(rust_paths)
}

fn extract_logger_invocations(
    path: &str,
    source: &str,
) -> Result<Vec<LoggerInvocation>, AuditError> {
    let syntax = syn::parse_file(source)
        .map_err(|_| AuditError::new(format!("failed to parse Rust logger input: {path}")))?;
    let mut visitor = LoggerVisitor::new(path);
    visitor.visit_file(&syntax);
    visitor.invocations.sort_by(|left, right| {
        (left.line, left.column, left.macro_name).cmp(&(right.line, right.column, right.macro_name))
    });
    for pair in visitor.invocations.windows(2) {
        let [left, right] = pair else {
            continue;
        };
        if left.id == right.id {
            return Err(AuditError::new(format!(
                "duplicate logger invocation source identity: {}",
                left.id
            )));
        }
    }
    Ok(visitor.invocations)
}

struct LoggerVisitor<'a> {
    path: &'a str,
    base_context: LoggerSourceContext,
    test_only: bool,
    invocations: Vec<LoggerInvocation>,
}

impl<'a> LoggerVisitor<'a> {
    fn new(path: &'a str) -> Self {
        Self {
            path,
            base_context: path_source_context(path),
            test_only: false,
            invocations: Vec::new(),
        }
    }

    fn push_attrs(&mut self, attrs: &[Attribute]) -> bool {
        let previous = self.test_only;
        self.test_only |= attrs_require_test(attrs);
        previous
    }

    fn restore_test_context(&mut self, previous: bool) {
        self.test_only = previous;
    }

    fn source_context(&self) -> LoggerSourceContext {
        if self.test_only {
            LoggerSourceContext::Test
        } else {
            self.base_context
        }
    }

    fn record_invocation(
        &mut self,
        macro_name: LoggerMacro,
        syntax: LoggerInvocationSyntax,
        span: proc_macro2::Span,
        normalized: &str,
    ) {
        let start = span.start();
        let line = start.line;
        let column = start.column + 1;
        self.invocations.push(LoggerInvocation {
            id: format!("logger-invocation:{}:{line}:{column}", self.path),
            path: self.path.to_string(),
            line,
            column,
            macro_name,
            syntax,
            source_context: self.source_context(),
            fingerprint: sha256_fingerprint(normalized.as_bytes()),
        });
    }
}

impl<'ast> Visit<'ast> for LoggerVisitor<'_> {
    fn visit_file(&mut self, node: &'ast syn::File) {
        let previous = self.push_attrs(&node.attrs);
        syn::visit::visit_file(self, node);
        self.restore_test_context(previous);
    }

    fn visit_item(&mut self, node: &'ast Item) {
        let previous = self.push_attrs(item_attrs(node));
        syn::visit::visit_item(self, node);
        self.restore_test_context(previous);
    }

    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        let previous = self.push_attrs(impl_item_attrs(node));
        syn::visit::visit_impl_item(self, node);
        self.restore_test_context(previous);
    }

    fn visit_trait_item(&mut self, node: &'ast TraitItem) {
        let previous = self.push_attrs(trait_item_attrs(node));
        syn::visit::visit_trait_item(self, node);
        self.restore_test_context(previous);
    }

    fn visit_foreign_item(&mut self, node: &'ast ForeignItem) {
        let previous = self.push_attrs(foreign_item_attrs(node));
        syn::visit::visit_foreign_item(self, node);
        self.restore_test_context(previous);
    }

    fn visit_expr(&mut self, node: &'ast Expr) {
        let previous = self.push_attrs(expr_attrs(node));
        syn::visit::visit_expr(self, node);
        self.restore_test_context(previous);
    }

    fn visit_stmt(&mut self, node: &'ast Stmt) {
        let previous = self.push_attrs(stmt_attrs(node));
        syn::visit::visit_stmt(self, node);
        self.restore_test_context(previous);
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        let previous = self.push_attrs(&node.attrs);
        syn::visit::visit_arm(self, node);
        self.restore_test_context(previous);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        for invocation in logger_invocations_in_tokens(&node.tokens) {
            self.record_invocation(
                invocation.macro_name,
                LoggerInvocationSyntax::MacroTemplate,
                invocation.span,
                &invocation.normalized,
            );
        }

        let Some(macro_name) = node
            .path
            .segments
            .last()
            .and_then(|segment| LoggerMacro::from_name(&segment.ident.to_string()))
        else {
            return;
        };
        let normalized = node.to_token_stream().to_string();
        self.record_invocation(
            macro_name,
            LoggerInvocationSyntax::Direct,
            node.path.span(),
            &normalized,
        );
    }
}

fn sha256_fingerprint(bytes: &[u8]) -> String {
    let hex_digit = |nibble: u8| {
        char::from(if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        })
    };
    let digest = Sha256::digest(bytes);
    let mut fingerprint = String::with_capacity(71);
    fingerprint.push_str("sha256:");
    for byte in digest {
        fingerprint.push(hex_digit(byte >> 4));
        fingerprint.push(hex_digit(byte & 0x0f));
    }
    fingerprint
}

fn path_source_context(path: &str) -> LoggerSourceContext {
    let components = path.split('/').collect::<Vec<_>>();
    if components.contains(&"tests") {
        LoggerSourceContext::Test
    } else if components.contains(&"examples") {
        LoggerSourceContext::Example
    } else {
        LoggerSourceContext::Production
    }
}

fn attrs_require_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        if attribute.path().is_ident("test") {
            return true;
        }
        if !attribute.path().is_ident("cfg") {
            return false;
        }
        attribute
            .parse_args::<Meta>()
            .is_ok_and(|predicate| meta_requires_test(&predicate))
    })
}

fn meta_requires_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(path) => path.is_ident("test"),
        Meta::List(list) if list.path.is_ident("all") => {
            parse_meta_list(&list.tokens).is_some_and(|items| items.iter().any(meta_requires_test))
        }
        Meta::List(list) if list.path.is_ident("any") => parse_meta_list(&list.tokens)
            .is_some_and(|items| !items.is_empty() && items.iter().all(meta_requires_test)),
        Meta::List(_) | Meta::NameValue(_) => false,
    }
}

fn parse_meta_list(tokens: &TokenStream) -> Option<Punctuated<Meta, Token![,]>> {
    Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(tokens.clone())
        .ok()
}

struct TokenInvocation {
    macro_name: LoggerMacro,
    span: proc_macro2::Span,
    normalized: String,
}

fn logger_invocations_in_tokens(tokens: &TokenStream) -> Vec<TokenInvocation> {
    let mut invocations = Vec::new();
    collect_logger_invocations_in_tokens(tokens, &mut invocations);
    invocations
}

fn collect_logger_invocations_in_tokens(
    tokens: &TokenStream,
    invocations: &mut Vec<TokenInvocation>,
) {
    let trees = tokens.clone().into_iter().collect::<Vec<_>>();
    for (index, tree) in trees.iter().enumerate() {
        if let TokenTree::Group(group) = tree {
            collect_logger_invocations_in_tokens(&group.stream(), invocations);
        }
        let Some(TokenTree::Punct(punctuation)) = trees.get(index) else {
            continue;
        };
        if punctuation.as_char() != '!'
            || trees
                .get(index + 1)
                .is_none_or(|tree| !matches!(tree, TokenTree::Group(_)))
        {
            continue;
        }
        let Some(TokenTree::Ident(identifier)) =
            index.checked_sub(1).and_then(|index| trees.get(index))
        else {
            continue;
        };
        let Some(macro_name) = LoggerMacro::from_name(&identifier.to_string()) else {
            continue;
        };
        let start = qualified_path_start(&trees, index - 1);
        let Some(invocation_tokens) = trees.get(start..=index + 1) else {
            continue;
        };
        let normalized = invocation_tokens
            .iter()
            .cloned()
            .collect::<TokenStream>()
            .to_string();
        invocations.push(TokenInvocation {
            macro_name,
            span: identifier.span(),
            normalized,
        });
    }
}

fn qualified_path_start(trees: &[TokenTree], final_ident: usize) -> usize {
    let mut start = final_ident;
    while start >= 3
        && matches!(trees.get(start - 1), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == ':')
        && matches!(trees.get(start - 2), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == ':')
        && matches!(trees.get(start - 3), Some(TokenTree::Ident(_)))
    {
        start -= 3;
    }
    if start >= 2
        && matches!(trees.get(start - 1), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == ':')
        && matches!(trees.get(start - 2), Some(TokenTree::Punct(punctuation)) if punctuation.as_char() == ':')
    {
        start -= 2;
    }
    start
}

fn logger_counts(
    scanned_rust_files: usize,
    matching_input_files: usize,
    invocations: &[LoggerInvocation],
) -> LoggerProducerCounts {
    let macro_count = |name| {
        invocations
            .iter()
            .filter(|invocation| invocation.macro_name == name)
            .count()
    };
    let context_count = |context| {
        invocations
            .iter()
            .filter(|invocation| invocation.source_context == context)
            .count()
    };
    LoggerProducerCounts {
        scanned_rust_files,
        matching_input_files,
        ordinary: invocations
            .iter()
            .filter(|invocation| !invocation.macro_name.is_unrestricted())
            .count(),
        unrestricted: invocations
            .iter()
            .filter(|invocation| invocation.macro_name.is_unrestricted())
            .count(),
        error: macro_count(LoggerMacro::Error),
        warn: macro_count(LoggerMacro::Warn),
        info: macro_count(LoggerMacro::Info),
        debug: macro_count(LoggerMacro::Debug),
        trace: macro_count(LoggerMacro::Trace),
        error_unrestricted: macro_count(LoggerMacro::ErrorUnrestricted),
        warn_unrestricted: macro_count(LoggerMacro::WarnUnrestricted),
        info_unrestricted: macro_count(LoggerMacro::InfoUnrestricted),
        production: context_count(LoggerSourceContext::Production),
        test: context_count(LoggerSourceContext::Test),
        example: context_count(LoggerSourceContext::Example),
        direct: invocations
            .iter()
            .filter(|invocation| invocation.syntax == LoggerInvocationSyntax::Direct)
            .count(),
        macro_template: invocations
            .iter()
            .filter(|invocation| invocation.syntax == LoggerInvocationSyntax::MacroTemplate)
            .count(),
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) | _ => &[],
    }
}

fn impl_item_attrs(item: &ImplItem) -> &[Attribute] {
    match item {
        ImplItem::Const(item) => &item.attrs,
        ImplItem::Fn(item) => &item.attrs,
        ImplItem::Type(item) => &item.attrs,
        ImplItem::Macro(item) => &item.attrs,
        ImplItem::Verbatim(_) | _ => &[],
    }
}

fn trait_item_attrs(item: &TraitItem) -> &[Attribute] {
    match item {
        TraitItem::Const(item) => &item.attrs,
        TraitItem::Fn(item) => &item.attrs,
        TraitItem::Type(item) => &item.attrs,
        TraitItem::Macro(item) => &item.attrs,
        TraitItem::Verbatim(_) | _ => &[],
    }
}

fn foreign_item_attrs(item: &ForeignItem) -> &[Attribute] {
    match item {
        ForeignItem::Fn(item) => &item.attrs,
        ForeignItem::Static(item) => &item.attrs,
        ForeignItem::Type(item) => &item.attrs,
        ForeignItem::Macro(item) => &item.attrs,
        ForeignItem::Verbatim(_) | _ => &[],
    }
}

fn stmt_attrs(statement: &Stmt) -> &[Attribute] {
    match statement {
        Stmt::Local(local) => &local.attrs,
        Stmt::Macro(statement) => &statement.attrs,
        Stmt::Item(_) | Stmt::Expr(_, _) => &[],
    }
}

fn expr_attrs(expression: &Expr) -> &[Attribute] {
    match expression {
        Expr::Array(expression) => &expression.attrs,
        Expr::Assign(expression) => &expression.attrs,
        Expr::Async(expression) => &expression.attrs,
        Expr::Await(expression) => &expression.attrs,
        Expr::Binary(expression) => &expression.attrs,
        Expr::Block(expression) => &expression.attrs,
        Expr::Break(expression) => &expression.attrs,
        Expr::Call(expression) => &expression.attrs,
        Expr::Cast(expression) => &expression.attrs,
        Expr::Closure(expression) => &expression.attrs,
        Expr::Const(expression) => &expression.attrs,
        Expr::Continue(expression) => &expression.attrs,
        Expr::Field(expression) => &expression.attrs,
        Expr::ForLoop(expression) => &expression.attrs,
        Expr::Group(expression) => &expression.attrs,
        Expr::If(expression) => &expression.attrs,
        Expr::Index(expression) => &expression.attrs,
        Expr::Infer(expression) => &expression.attrs,
        Expr::Let(expression) => &expression.attrs,
        Expr::Lit(expression) => &expression.attrs,
        Expr::Loop(expression) => &expression.attrs,
        Expr::Macro(expression) => &expression.attrs,
        Expr::Match(expression) => &expression.attrs,
        Expr::MethodCall(expression) => &expression.attrs,
        Expr::Paren(expression) => &expression.attrs,
        Expr::Path(expression) => &expression.attrs,
        Expr::Range(expression) => &expression.attrs,
        Expr::RawAddr(expression) => &expression.attrs,
        Expr::Reference(expression) => &expression.attrs,
        Expr::Repeat(expression) => &expression.attrs,
        Expr::Return(expression) => &expression.attrs,
        Expr::Struct(expression) => &expression.attrs,
        Expr::Try(expression) => &expression.attrs,
        Expr::TryBlock(expression) => &expression.attrs,
        Expr::Tuple(expression) => &expression.attrs,
        Expr::Unary(expression) => &expression.attrs,
        Expr::Unsafe(expression) => &expression.attrs,
        Expr::While(expression) => &expression.attrs,
        Expr::Yield(expression) => &expression.attrs,
        Expr::Verbatim(_) | _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(path: &str, source: &str) -> Vec<LoggerInvocation> {
        extract_logger_invocations(path, source).expect("fixture must extract")
    }

    #[test]
    fn extracts_exact_qualified_and_multiline_invocations() {
        let invocations = extract(
            "src/vmm/src/sample.rs",
            r#"
fn run() {
    error!("one");
    crate::logger::warn_unrestricted!(
        "two"
    );
    log::trace!("three");
    compile_error!("not a logger call");
    __log_error!("not public");
}
"#,
        );
        assert_eq!(invocations.len(), 3);
        assert_eq!(invocations[0].macro_name, LoggerMacro::Error);
        assert_eq!(invocations[1].macro_name, LoggerMacro::WarnUnrestricted);
        assert_eq!(invocations[2].macro_name, LoggerMacro::Trace);
        assert!(
            invocations
                .iter()
                .all(|invocation| invocation.syntax == LoggerInvocationSyntax::Direct)
        );
        assert!(
            invocations
                .iter()
                .all(|invocation| invocation.source_context == LoggerSourceContext::Production)
        );
    }

    #[test]
    fn ignores_logger_wrapper_definitions() {
        let invocations = extract(
            "src/vmm/src/logger.rs",
            r#"
macro_rules! error {
    ($($arg:tt)+) => { $crate::logger::__log_error!($($arg)+) };
}
"#,
        );
        assert!(invocations.is_empty());
    }

    #[test]
    fn extracts_target_invocations_in_nonlogger_macro_templates() {
        let source = r#"fn run() { outer!(error!("secret-value")); }"#;
        let invocations = extract_logger_invocations("src/vmm/src/sample.rs", source)
            .expect("macro-template producer must extract");
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0].macro_name, LoggerMacro::Error);
        assert_eq!(invocations[0].syntax, LoggerInvocationSyntax::MacroTemplate);
        assert!(!invocations[0].fingerprint.contains("secret-value"));
    }

    #[test]
    fn keeps_direct_and_nested_sites_distinct_and_ignores_text() {
        let invocations = extract(
            "src/vmm/src/sample.rs",
            r#"
fn run() {
    error!("same");
    outer!(error!("same"));
    let _text = "warn!(\"not syntax\")";
    // info!("not syntax");
}
"#,
        );
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].syntax, LoggerInvocationSyntax::Direct);
        assert_eq!(invocations[1].syntax, LoggerInvocationSyntax::MacroTemplate);
        assert_ne!(invocations[0].id, invocations[1].id);
        assert_eq!(invocations[0].fingerprint, invocations[1].fingerprint);
    }

    #[test]
    fn derives_test_and_example_contexts_from_syntax_and_paths() {
        let invocations = extract(
            "src/vmm/src/sample.rs",
            r#"
#[cfg(test)]
mod tests {
    fn test_call() { warn!("test"); }
}

#[cfg(any(test, target_os = "linux"))]
fn production_possible() { info!("production"); }

#[cfg(all(target_arch = "aarch64", test))]
fn test_required() { debug!("test"); }
"#,
        );
        assert_eq!(invocations[0].source_context, LoggerSourceContext::Test);
        assert_eq!(
            invocations[1].source_context,
            LoggerSourceContext::Production
        );
        assert_eq!(invocations[2].source_context, LoggerSourceContext::Test);

        let examples = extract(
            "src/log-instrument/examples/one.rs",
            "fn main() { info!(\"example\"); }",
        );
        assert_eq!(examples[0].source_context, LoggerSourceContext::Example);
    }

    #[test]
    fn fingerprints_normalize_formatting_and_change_with_tokens() {
        let compact = extract("src/vmm/src/sample.rs", "fn f(){error!(\"one\");}");
        let spaced = extract("src/vmm/src/sample.rs", "fn f() { error! ( \"one\" ) ; }");
        let changed = extract("src/vmm/src/sample.rs", "fn f(){error!(\"two\");}");
        assert_eq!(compact[0].fingerprint, spaced[0].fingerprint);
        assert_ne!(compact[0].fingerprint, changed[0].fingerprint);
        assert!(compact[0].fingerprint.starts_with("sha256:"));
        assert_eq!(compact[0].fingerprint.len(), 71);
    }
}
