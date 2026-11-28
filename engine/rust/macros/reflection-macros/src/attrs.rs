//! Parsing of the `#[reflect(...)]` attributes.

use syn::meta::ParseNestedMeta;
use syn::{Attribute, Expr, LitStr, Path, Result};

/// `#[reflect(...)]` on the type.
#[derive(Default)]
pub(crate) struct ContainerAttrs {
    pub(crate) name: Option<LitStr>,
    pub(crate) aliases: Vec<LitStr>,
    /// Type data to register: `Default`, `Clone`, `PartialEq`, `Debug`, `Hash` or the path of a type that
    /// implements `FromType`.
    pub(crate) data: Vec<Path>,
    /// `no_from_reflect`: do not write `Typed::from_reflect`.
    pub(crate) no_from_reflect: bool,
    pub(crate) meta: MetaAttrs,
}

/// `#[reflect(...)]` on a field.
#[derive(Default)]
pub(crate) struct FieldAttrs {
    pub(crate) skip: bool,
    /// `default`: `from_reflect` uses `Default::default()` when the source has no such field.
    pub(crate) default: bool,
    pub(crate) rename: Option<LitStr>,
    pub(crate) aliases: Vec<LitStr>,
    pub(crate) meta: MetaAttrs,
}

/// `#[reflect(...)]` on an enum variant.
#[derive(Default)]
pub(crate) struct VariantAttrs {
    pub(crate) rename: Option<LitStr>,
    pub(crate) aliases: Vec<LitStr>,
    pub(crate) meta: MetaAttrs,
}

/// The metadata attributes, the same set of parsing for types, fields and variants.
#[derive(Default)]
pub(crate) struct MetaAttrs {
    pub(crate) tooltip: Option<LitStr>,
    pub(crate) category: Option<LitStr>,
    /// `range(min = .., max = ..)`: the two expressions.
    pub(crate) range: Option<(Expr, Expr)>,
    pub(crate) readonly: bool,
    pub(crate) hidden: bool,
    pub(crate) transient: bool,
    /// `attr = <expression>`: typed custom attributes.
    pub(crate) custom: Vec<Expr>,
    /// The text of the doc comments (only with the `docs` feature).
    pub(crate) docs: Option<String>,
}

/// Where a metadata attribute is written; decides which ones are accepted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Type,
    Field,
    Variant,
}

/// Reads `= "..."` after an attribute name; the string must not be empty. Used inside the closures given to
/// `parse_nested_meta`, so `?` and `return` refer to the closure.
macro_rules! string_value {
    ($meta:ident) => {{
        let value: LitStr = $meta.value()?.parse()?;
        if value.value().is_empty() {
            return Err($meta.error("the value must not be empty"));
        }
        value
    }};
}

/// Reads one metadata attribute (`tooltip`, `category`, `hidden`, `readonly`, `transient`, `range(..)`,
/// `attr = ..`). Returns `Ok(false)` when `meta` is not one of them (or not allowed in this scope), so the
/// caller can try its own attributes.
fn metadata_attr(out: &mut MetaAttrs, meta: &ParseNestedMeta<'_>, scope: Scope) -> Result<bool> {
    let path = &meta.path;
    if path.is_ident("tooltip") {
        if out.tooltip.is_some() {
            return Err(meta.error("`tooltip` is given twice"));
        }
        out.tooltip = Some(string_value!(meta));
    } else if path.is_ident("category") {
        if out.category.is_some() {
            return Err(meta.error("`category` is given twice"));
        }
        out.category = Some(string_value!(meta));
    } else if path.is_ident("hidden") {
        out.hidden = true;
    } else if scope != Scope::Variant && path.is_ident("readonly") {
        out.readonly = true;
    } else if scope != Scope::Variant && path.is_ident("transient") {
        out.transient = true;
    } else if scope == Scope::Field && path.is_ident("range") {
        if out.range.is_some() {
            return Err(meta.error("`range` is given twice"));
        }
        let mut min = None;
        let mut max = None;
        meta.parse_nested_meta(|inner| {
            if inner.path.is_ident("min") {
                min = Some(inner.value()?.parse::<Expr>()?);
                Ok(())
            } else if inner.path.is_ident("max") {
                max = Some(inner.value()?.parse::<Expr>()?);
                Ok(())
            } else {
                Err(inner.error("expected `min = ...` or `max = ...`"))
            }
        })?;
        match (min, max) {
            (Some(min), Some(max)) => out.range = Some((min, max)),
            _ => return Err(meta.error("`range` needs both `min = ...` and `max = ...`")),
        }
    } else if path.is_ident("attr") {
        let value: Expr = meta.value()?.parse()?;
        out.custom.push(value);
    } else {
        return Ok(false);
    }
    Ok(true)
}

/// The doc comments of an item as one text (lines joined with `\n`, one leading space of each line removed);
/// `None` without the `docs` feature or without doc comments.
#[cfg(feature = "docs")]
fn docs_text(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("doc")
            && let syn::Meta::NameValue(name_value) = &attr.meta
            && let Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(text),
                ..
            }) = &name_value.value
        {
            let line = text.value();
            lines.push(line.strip_prefix(' ').unwrap_or(&line).to_owned());
        }
    }
    let text = lines.join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(not(feature = "docs"))]
fn docs_text(attrs: &[Attribute]) -> Option<String> {
    let _ = attrs;
    None
}

pub(crate) fn container(attrs: &[Attribute]) -> Result<ContainerAttrs> {
    let mut out = ContainerAttrs::default();
    out.meta.docs = docs_text(attrs);
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            if metadata_attr(&mut out.meta, &meta, Scope::Type)? {
                Ok(())
            } else if meta.path.is_ident("name") {
                if out.name.is_some() {
                    return Err(meta.error("`name` is given twice"));
                }
                out.name = Some(string_value!(meta));
                Ok(())
            } else if meta.path.is_ident("alias") {
                out.aliases.push(string_value!(meta));
                Ok(())
            } else if meta.path.is_ident("no_from_reflect") {
                out.no_from_reflect = true;
                Ok(())
            } else if meta
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident.to_string().starts_with(char::is_uppercase))
            {
                out.data.push(meta.path.clone());
                Ok(())
            } else {
                Err(meta.error(
                    "unsupported `reflect` attribute on a type; expected `name = \"...\"`, `alias = \"...\"`, `no_from_reflect`, `tooltip = \"...\"`, `category = \"...\"`, `hidden`, `readonly`, `transient`, `attr = <expression>` or the name of a type data (`Default`, `Clone`, `PartialEq`, `Debug`, `Hash`, or a type that implements `FromType`)",
                ))
            }
        })?;
    }
    Ok(out)
}

pub(crate) fn field(attrs: &[Attribute]) -> Result<FieldAttrs> {
    let mut out = FieldAttrs::default();
    out.meta.docs = docs_text(attrs);
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            if metadata_attr(&mut out.meta, &meta, Scope::Field)? {
                Ok(())
            } else if meta.path.is_ident("skip") {
                out.skip = true;
                Ok(())
            } else if meta.path.is_ident("default") {
                out.default = true;
                Ok(())
            } else if meta.path.is_ident("rename") {
                if out.rename.is_some() {
                    return Err(meta.error("`rename` is given twice"));
                }
                out.rename = Some(string_value!(meta));
                Ok(())
            } else if meta.path.is_ident("alias") {
                out.aliases.push(string_value!(meta));
                Ok(())
            } else {
                Err(meta.error(
                    "unsupported `reflect` attribute on a field; expected `skip`, `default`, `rename = \"...\"`, `alias = \"...\"`, `tooltip = \"...\"`, `category = \"...\"`, `range(min = .., max = ..)`, `hidden`, `readonly`, `transient` or `attr = <expression>`",
                ))
            }
        })?;
    }
    Ok(out)
}

pub(crate) fn variant(attrs: &[Attribute]) -> Result<VariantAttrs> {
    let mut out = VariantAttrs::default();
    out.meta.docs = docs_text(attrs);
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            if metadata_attr(&mut out.meta, &meta, Scope::Variant)? {
                Ok(())
            } else if meta.path.is_ident("rename") {
                if out.rename.is_some() {
                    return Err(meta.error("`rename` is given twice"));
                }
                out.rename = Some(string_value!(meta));
                Ok(())
            } else if meta.path.is_ident("alias") {
                out.aliases.push(string_value!(meta));
                Ok(())
            } else {
                Err(meta.error(
                    "unsupported `reflect` attribute on a variant; expected `rename = \"...\"`, `alias = \"...\"`, `tooltip = \"...\"`, `category = \"...\"`, `hidden` or `attr = <expression>`",
                ))
            }
        })?;
    }
    Ok(out)
}
