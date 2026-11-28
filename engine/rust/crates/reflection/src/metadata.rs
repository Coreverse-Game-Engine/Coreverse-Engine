//! Metadata of types, fields and enum variants (milestone R5).
//!
//! [`Metadata`] is what an editor or a serializer needs to know about a thing besides its shape: a tooltip, a
//! category, a numeric range, whether it is read-only, hidden or transient, the documentation comment and any
//! number of *typed custom attributes* (ordinary Rust values, found again by their type).
//!
//! Metadata is only *information*. The reflection operations (`apply`, `path_set`, `reflect_mut`, ...) do not
//! look at it: a `readonly` field can still be written through reflection. Editors and serializers decide what
//! to do with it.
//!
//! The derive fills it from `#[reflect(...)]` attributes, see [`Reflect`](crate::Reflect). By hand:
//!
//! ```
//! use reflection::Metadata;
//!
//! let metadata = Metadata::new()
//!     .with_tooltip("Hit points")
//!     .with_range(0.0, 100.0)
//!     .with_custom(7_u32);
//! assert_eq!(metadata.tooltip(), Some("Hit points"));
//! assert_eq!(metadata.range().map(|range| range.max), Some(100.0));
//! assert_eq!(metadata.custom::<u32>(), Some(&7));
//! ```

use std::any::{Any, TypeId, type_name};
use std::fmt;
use std::sync::Arc;

/// An inclusive range of numbers, for example the allowed values of a slider.
///
/// `min <= max` is expected but not checked; [`ValueRange::clamp`] never panics either way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueRange {
    /// The smallest value.
    pub min: f64,
    /// The largest value.
    pub max: f64,
}

impl ValueRange {
    /// Whether `value` lies in the range (bounds included).
    pub fn contains(&self, value: f64) -> bool {
        (self.min..=self.max).contains(&value)
    }

    /// The value moved into the range. Unlike [`f64::clamp`] this does not panic when the bounds are in the
    /// wrong order or are NaN.
    pub fn clamp(&self, value: f64) -> f64 {
        value.max(self.min).min(self.max)
    }
}

/// One typed custom attribute.
#[derive(Clone)]
struct CustomEntry {
    id: TypeId,
    name: &'static str,
    value: Arc<dyn Any + Send + Sync>,
}

/// Metadata of a type, a field or an enum variant.
///
/// Cheap to clone: custom attributes are shared, not copied. Which parts the derive accepts where:
///
/// | part | type | field | variant |
/// |---|---|---|---|
/// | documentation (`docs` feature) | yes | yes | yes |
/// | `tooltip`, `category`, `hidden`, `attr` | yes | yes | yes |
/// | `readonly`, `transient` | yes | yes | no |
/// | `range` | no | yes | no |
/// | `default` | no | yes | no |
#[derive(Clone, Default)]
pub struct Metadata {
    docs: Option<&'static str>,
    tooltip: Option<&'static str>,
    category: Option<&'static str>,
    range: Option<ValueRange>,
    readonly: bool,
    hidden: bool,
    transient: bool,
    has_default: bool,
    custom: Vec<CustomEntry>,
}

impl Metadata {
    /// Empty metadata.
    pub const fn new() -> Self {
        Self {
            docs: None,
            tooltip: None,
            category: None,
            range: None,
            readonly: false,
            hidden: false,
            transient: false,
            has_default: false,
            custom: Vec::new(),
        }
    }

    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        self.docs.is_none()
            && self.tooltip.is_none()
            && self.category.is_none()
            && self.range.is_none()
            && !self.readonly
            && !self.hidden
            && !self.transient
            && !self.has_default
            && self.custom.is_empty()
    }

    // ---- builders ----

    /// With the documentation text (what the doc comments said; the derive only captures it when the
    /// `docs` feature is on).
    #[must_use]
    pub fn with_docs(mut self, docs: &'static str) -> Self {
        self.docs = Some(docs);
        self
    }

    /// With a short help text for editors.
    #[must_use]
    pub fn with_tooltip(mut self, tooltip: &'static str) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// With the category (an editor groups things by it).
    #[must_use]
    pub fn with_category(mut self, category: &'static str) -> Self {
        self.category = Some(category);
        self
    }

    /// With an inclusive numeric range. Any numbers that convert into `f64` do (`0`, `0.5`, `-3_i16`, ...).
    #[must_use]
    pub fn with_range<A: Into<f64>, B: Into<f64>>(mut self, min: A, max: B) -> Self {
        self.range = Some(ValueRange {
            min: min.into(),
            max: max.into(),
        });
        self
    }

    /// Marked read-only: an editor shows the value but does not let it be changed.
    #[must_use]
    pub fn with_readonly(mut self) -> Self {
        self.readonly = true;
        self
    }

    /// Marked hidden: an editor does not show it.
    #[must_use]
    pub fn with_hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Marked transient: a serializer does not write it (unlike `skip`, it stays visible to reflection).
    #[must_use]
    pub fn with_transient(mut self) -> Self {
        self.transient = true;
        self
    }

    /// Marked as having a default value to fall back on (`#[reflect(default)]` on a field), so data may omit it.
    #[must_use]
    pub fn with_default(mut self) -> Self {
        self.has_default = true;
        self
    }

    /// With a custom attribute. There is one value per type: giving a second value of the same type replaces
    /// the first.
    #[must_use]
    pub fn with_custom<T: Any + Send + Sync>(mut self, value: T) -> Self {
        let id = TypeId::of::<T>();
        let entry = CustomEntry {
            id,
            name: type_name::<T>(),
            value: Arc::new(value),
        };
        match self.custom.iter_mut().find(|existing| existing.id == id) {
            Some(slot) => *slot = entry,
            None => self.custom.push(entry),
        }
        self
    }

    // ---- readers ----

    /// The documentation text, if it was captured.
    pub fn docs(&self) -> Option<&'static str> {
        self.docs
    }

    /// The tooltip.
    pub fn tooltip(&self) -> Option<&'static str> {
        self.tooltip
    }

    /// The category.
    pub fn category(&self) -> Option<&'static str> {
        self.category
    }

    /// The numeric range.
    pub fn range(&self) -> Option<ValueRange> {
        self.range
    }

    /// Whether it is read-only.
    pub fn is_readonly(&self) -> bool {
        self.readonly
    }

    /// Whether it is hidden.
    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// Whether it is transient.
    pub fn is_transient(&self) -> bool {
        self.transient
    }

    /// Whether `#[reflect(default)]` was given.
    pub fn has_default(&self) -> bool {
        self.has_default
    }

    /// The custom attribute of type `T`, if there is one.
    pub fn custom<T: Any + Send + Sync>(&self) -> Option<&T> {
        let id = TypeId::of::<T>();
        self.custom
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.value.downcast_ref::<T>())
    }

    /// Whether there is a custom attribute of type `T`.
    pub fn has_custom<T: Any + Send + Sync>(&self) -> bool {
        let id = TypeId::of::<T>();
        self.custom.iter().any(|entry| entry.id == id)
    }

    /// The number of custom attributes.
    pub fn custom_len(&self) -> usize {
        self.custom.len()
    }

    /// The type names of the custom attributes, for diagnostics only (`type_name` is not stable between
    /// compiler versions).
    pub fn custom_type_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.custom.iter().map(|entry| entry.name)
    }
}

impl fmt::Debug for Metadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let custom: Vec<&str> = self.custom_type_names().collect();
        f.debug_struct("Metadata")
            .field("docs", &self.docs)
            .field("tooltip", &self.tooltip)
            .field("category", &self.category)
            .field("range", &self.range)
            .field("readonly", &self.readonly)
            .field("hidden", &self.hidden)
            .field("transient", &self.transient)
            .field("has_default", &self.has_default)
            .field("custom", &custom)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Step(f32);

    #[derive(Debug, PartialEq)]
    struct Label(&'static str);

    #[test]
    fn new_is_empty() {
        assert!(Metadata::new().is_empty());
        assert!(Metadata::default().is_empty());
        assert!(!Metadata::new().with_hidden().is_empty());
    }

    #[test]
    fn builders_set_what_readers_return() {
        let metadata = Metadata::new()
            .with_docs("docs")
            .with_tooltip("tip")
            .with_category("cat")
            .with_range(-1, 2.5)
            .with_readonly()
            .with_hidden()
            .with_transient()
            .with_default();
        assert_eq!(metadata.docs(), Some("docs"));
        assert_eq!(metadata.tooltip(), Some("tip"));
        assert_eq!(metadata.category(), Some("cat"));
        assert_eq!(
            metadata.range(),
            Some(ValueRange {
                min: -1.0,
                max: 2.5
            })
        );
        assert!(metadata.is_readonly());
        assert!(metadata.is_hidden());
        assert!(metadata.is_transient());
        assert!(metadata.has_default());
        assert!(!metadata.is_empty());
    }

    #[test]
    fn custom_attributes_are_found_by_type() {
        let metadata = Metadata::new()
            .with_custom(Step(0.5))
            .with_custom(Label("hp"));
        assert_eq!(metadata.custom_len(), 2);
        assert_eq!(metadata.custom::<Step>(), Some(&Step(0.5)));
        assert_eq!(metadata.custom::<Label>(), Some(&Label("hp")));
        assert!(metadata.has_custom::<Step>());
        assert!(!metadata.has_custom::<u8>());
        assert_eq!(metadata.custom::<u8>(), None);
    }

    #[test]
    fn same_custom_type_replaces() {
        let metadata = Metadata::new()
            .with_custom(Step(1.0))
            .with_custom(Step(2.0));
        assert_eq!(metadata.custom_len(), 1);
        assert_eq!(metadata.custom::<Step>(), Some(&Step(2.0)));
    }

    #[test]
    fn clones_share_custom_values() {
        let metadata = Metadata::new().with_custom(Step(3.0));
        let copy = metadata.clone();
        assert_eq!(copy.custom::<Step>(), Some(&Step(3.0)));
    }

    #[test]
    fn range_contains_and_clamp() {
        let range = ValueRange {
            min: 0.0,
            max: 10.0,
        };
        assert!(range.contains(0.0));
        assert!(range.contains(10.0));
        assert!(!range.contains(10.5));
        assert_eq!(range.clamp(-4.0), 0.0);
        assert_eq!(range.clamp(99.0), 10.0);
        assert_eq!(range.clamp(5.0), 5.0);
        // wrong order must not panic
        let backwards = ValueRange { min: 5.0, max: 1.0 };
        let _ = backwards.clamp(3.0);
    }

    #[test]
    fn debug_lists_custom_type_names() {
        let text = format!("{:?}", Metadata::new().with_custom(Step(1.0)));
        assert!(text.contains("Step"), "{text}");
    }
}
