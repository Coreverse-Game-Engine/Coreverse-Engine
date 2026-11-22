//! Parsing of the `#[reflect(...)]` attributes.

use syn::{Attribute, LitStr, Result};

/// `#[reflect(...)]` on the type.
#[derive(Default)]
pub(crate) struct ContainerAttrs {
    pub(crate) name: Option<LitStr>,
    pub(crate) aliases: Vec<LitStr>,
}

/// `#[reflect(...)]` on a field.
#[derive(Default)]
pub(crate) struct FieldAttrs {
    pub(crate) skip: bool,
    pub(crate) rename: Option<LitStr>,
    pub(crate) aliases: Vec<LitStr>,
}

/// `#[reflect(...)]` on an enum variant.
#[derive(Default)]
pub(crate) struct VariantAttrs {
    pub(crate) rename: Option<LitStr>,
    pub(crate) aliases: Vec<LitStr>,
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

pub(crate) fn container(attrs: &[Attribute]) -> Result<ContainerAttrs> {
    let mut out = ContainerAttrs::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                if out.name.is_some() {
                    return Err(meta.error("`name` is given twice"));
                }
                out.name = Some(string_value!(meta));
                Ok(())
            } else if meta.path.is_ident("alias") {
                out.aliases.push(string_value!(meta));
                Ok(())
            } else {
                Err(meta.error(
                    "unsupported `reflect` attribute on a type; expected `name = \"...\"` or `alias = \"...\"`",
                ))
            }
        })?;
    }
    Ok(out)
}

pub(crate) fn field(attrs: &[Attribute]) -> Result<FieldAttrs> {
    let mut out = FieldAttrs::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                out.skip = true;
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
                    "unsupported `reflect` attribute on a field; expected `skip`, `rename = \"...\"` or `alias = \"...\"`",
                ))
            }
        })?;
    }
    Ok(out)
}

pub(crate) fn variant(attrs: &[Attribute]) -> Result<VariantAttrs> {
    let mut out = VariantAttrs::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("reflect")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
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
                    "unsupported `reflect` attribute on a variant; expected `rename = \"...\"` or `alias = \"...\"`",
                ))
            }
        })?;
    }
    Ok(out)
}
